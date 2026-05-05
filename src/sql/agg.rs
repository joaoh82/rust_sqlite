//! SQLR-3 aggregate runtime.
//!
//! Three concerns live here:
//!   1. `AggState` — per-group accumulator state for COUNT/SUM/AVG/MIN/MAX,
//!      with SQLite-style numeric type rules (Sum stays Integer until a
//!      Real input or i64 overflow forces a one-time promotion to f64).
//!   2. `DistinctKey` — a hashable typed wrapper around `Value`, used both
//!      as the per-row key for GROUP BY and as the dedupe key for
//!      `COUNT(DISTINCT col)` and `SELECT DISTINCT`.
//!   3. `like_match` — the iterative two-pointer LIKE matcher (case
//!      insensitive ASCII to match SQLite's default).
//!
//! All of this is pure-functional in the sense that nothing here touches
//! the `Database`/`Table`. The executor walks rows and feeds values in.

use std::collections::HashSet;

use crate::sql::db::table::Value;
use crate::sql::parser::select::{AggregateArg, AggregateCall, AggregateFn};

/// SQLite-style numeric accumulator: stays `Int` while every input is
/// Integer and the running total fits in i64, otherwise promotes once to
/// `Real` and never demotes back.
#[derive(Debug, Clone)]
pub enum SumAcc {
    Int(i64),
    Real(f64),
}

impl SumAcc {
    fn add_int(&mut self, j: i64) {
        match *self {
            SumAcc::Int(i) => match i.checked_add(j) {
                Some(s) => *self = SumAcc::Int(s),
                None => *self = SumAcc::Real(i as f64 + j as f64),
            },
            SumAcc::Real(r) => *self = SumAcc::Real(r + j as f64),
        }
    }
    fn add_real(&mut self, r: f64) {
        match *self {
            SumAcc::Int(i) => *self = SumAcc::Real(i as f64 + r),
            SumAcc::Real(x) => *self = SumAcc::Real(x + r),
        }
    }
    fn as_value(&self) -> Value {
        match self {
            SumAcc::Int(i) => Value::Integer(*i),
            SumAcc::Real(r) => Value::Real(*r),
        }
    }
    fn as_f64(&self) -> f64 {
        match self {
            SumAcc::Int(i) => *i as f64,
            SumAcc::Real(r) => *r,
        }
    }
}

/// Per-aggregate accumulator. One instance per (group, projection-slot)
/// pair lives for the duration of the SELECT.
#[derive(Debug, Clone)]
pub enum AggState {
    /// `COUNT(*)` — counts every row, including all-NULL rows.
    CountStar(i64),
    /// `COUNT(col)` — counts non-NULL values, optionally with DISTINCT.
    Count {
        non_null: i64,
        distinct: Option<HashSet<DistinctKey>>,
    },
    /// `SUM(col)` — skips NULLs; `all_null` tracks the SQL semantic that
    /// SUM over an all-NULL or empty set yields NULL (not 0).
    Sum {
        acc: SumAcc,
        all_null: bool,
    },
    /// `AVG(col)` — always returns Real (or NULL on empty / all-NULL).
    Avg {
        acc: SumAcc,
        n: i64,
    },
    /// `MIN(col)` / `MAX(col)` — track the running winner (or None until
    /// the first non-NULL input).
    Min(Option<Value>),
    Max(Option<Value>),
}

impl AggState {
    /// Construct the initial accumulator for an aggregate call.
    pub fn new(call: &AggregateCall) -> Self {
        match call.func {
            AggregateFn::Count => match &call.arg {
                AggregateArg::Star => AggState::CountStar(0),
                AggregateArg::Column(_) => AggState::Count {
                    non_null: 0,
                    distinct: if call.distinct {
                        Some(HashSet::new())
                    } else {
                        None
                    },
                },
            },
            AggregateFn::Sum => AggState::Sum {
                acc: SumAcc::Int(0),
                all_null: true,
            },
            AggregateFn::Avg => AggState::Avg {
                acc: SumAcc::Int(0),
                n: 0,
            },
            AggregateFn::Min => AggState::Min(None),
            AggregateFn::Max => AggState::Max(None),
        }
    }

    /// Fold one row's value into the accumulator.
    /// For `COUNT(*)`, the value is irrelevant — pass anything.
    pub fn update(&mut self, value: &Value) -> crate::error::Result<()> {
        match self {
            AggState::CountStar(c) => *c += 1,
            AggState::Count { non_null, distinct } => {
                if !matches!(value, Value::Null) {
                    if let Some(set) = distinct {
                        set.insert(DistinctKey::from_value(value));
                    } else {
                        *non_null += 1;
                    }
                }
            }
            AggState::Sum { acc, all_null } => match value {
                Value::Null => {}
                Value::Integer(i) => {
                    *all_null = false;
                    acc.add_int(*i);
                }
                Value::Real(r) => {
                    *all_null = false;
                    acc.add_real(*r);
                }
                Value::Bool(b) => {
                    *all_null = false;
                    acc.add_int(if *b { 1 } else { 0 });
                }
                other => {
                    return Err(crate::error::SQLRiteError::Internal(format!(
                        "SUM expects a numeric column, got {}",
                        other.to_display_string()
                    )));
                }
            },
            AggState::Avg { acc, n } => match value {
                Value::Null => {}
                Value::Integer(i) => {
                    acc.add_int(*i);
                    *n += 1;
                }
                Value::Real(r) => {
                    acc.add_real(*r);
                    *n += 1;
                }
                Value::Bool(b) => {
                    acc.add_int(if *b { 1 } else { 0 });
                    *n += 1;
                }
                other => {
                    return Err(crate::error::SQLRiteError::Internal(format!(
                        "AVG expects a numeric column, got {}",
                        other.to_display_string()
                    )));
                }
            },
            AggState::Min(cur) => {
                if !matches!(value, Value::Null) {
                    match cur {
                        None => *cur = Some(value.clone()),
                        Some(c) => {
                            if compare_values_total(value, c).is_lt() {
                                *cur = Some(value.clone());
                            }
                        }
                    }
                }
            }
            AggState::Max(cur) => {
                if !matches!(value, Value::Null) {
                    match cur {
                        None => *cur = Some(value.clone()),
                        Some(c) => {
                            if compare_values_total(value, c).is_gt() {
                                *cur = Some(value.clone());
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Produce the final SQL value emitted for this group.
    pub fn finalize(&self) -> Value {
        match self {
            AggState::CountStar(c) => Value::Integer(*c),
            AggState::Count { non_null, distinct } => match distinct {
                Some(set) => Value::Integer(set.len() as i64),
                None => Value::Integer(*non_null),
            },
            AggState::Sum { acc, all_null } => {
                if *all_null {
                    Value::Null
                } else {
                    acc.as_value()
                }
            }
            AggState::Avg { acc, n } => {
                if *n == 0 {
                    Value::Null
                } else {
                    Value::Real(acc.as_f64() / (*n as f64))
                }
            }
            AggState::Min(v) | AggState::Max(v) => v.clone().unwrap_or(Value::Null),
        }
    }
}

/// A hashable typed wrapper around `Value`, used as the GROUP BY key
/// element and as the `COUNT(DISTINCT col)` set entry. We can't `impl
/// Hash for Value` because Value has a `Real(f64)` variant and `f64`
/// isn't `Hash + Eq`. Round-trip via `f64::to_bits` to keep the
/// canonical bit-pattern as the key — NaN keys remain distinguishable
/// by exact bit pattern, which is the safer choice for grouping (we
/// don't try to be cute about NaN==NaN).
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub enum DistinctKey {
    Null,
    Bool(bool),
    Int(i64),
    Real(u64),
    Text(String),
    Vector(Vec<u8>),
}

impl DistinctKey {
    pub fn from_value(v: &Value) -> Self {
        match v {
            Value::Null => DistinctKey::Null,
            Value::Bool(b) => DistinctKey::Bool(*b),
            Value::Integer(i) => DistinctKey::Int(*i),
            Value::Real(r) => DistinctKey::Real(r.to_bits()),
            Value::Text(s) => DistinctKey::Text(s.clone()),
            Value::Vector(v) => {
                let mut bytes = Vec::with_capacity(v.len() * 4);
                for f in v {
                    bytes.extend_from_slice(&f.to_le_bytes());
                }
                DistinctKey::Vector(bytes)
            }
        }
    }
}

/// Total-order comparison used by MIN/MAX. Mirrors the executor's
/// `compare_values` semantics (Int↔Real cross-coerce; otherwise stringify).
/// Kept separate to avoid a dependency from this module back into
/// executor.rs's private comparator.
fn compare_values_total(a: &Value, b: &Value) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a, b) {
        (Value::Null, Value::Null) => Ordering::Equal,
        (Value::Null, _) => Ordering::Less,
        (_, Value::Null) => Ordering::Greater,
        (Value::Integer(x), Value::Integer(y)) => x.cmp(y),
        (Value::Real(x), Value::Real(y)) => x.partial_cmp(y).unwrap_or(Ordering::Equal),
        (Value::Integer(x), Value::Real(y)) => {
            (*x as f64).partial_cmp(y).unwrap_or(Ordering::Equal)
        }
        (Value::Real(x), Value::Integer(y)) => {
            x.partial_cmp(&(*y as f64)).unwrap_or(Ordering::Equal)
        }
        (Value::Text(x), Value::Text(y)) => x.cmp(y),
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        (x, y) => x.to_display_string().cmp(&y.to_display_string()),
    }
}

/// SQL `LIKE` matcher.
///
/// Wildcards: `%` matches any (possibly empty) char sequence; `_`
/// matches exactly one char. `\` escapes the next char (so `\%` matches
/// a literal percent). When `case_insensitive` is true, ASCII letters
/// fold; non-ASCII characters compare by code-point (we don't pull in
/// Unicode case folding for v1).
///
/// Iterative two-pointer with backtracking — no recursion, so adversarial
/// patterns like `%a%a%a%a%a%b` against `aaaa…aa` can't blow the stack.
/// Worst case is O(|text| · |pattern|).
pub fn like_match(text: &str, pattern: &str, case_insensitive: bool) -> bool {
    let text: Vec<char> = text.chars().collect();
    let pat: Vec<char> = pattern.chars().collect();
    let n = text.len();
    let m = pat.len();

    let mut ti = 0usize;
    let mut pi = 0usize;
    // Backtrack point: the last position where we saw `%` and committed to
    // matching zero characters with it.
    let mut star_ti: Option<usize> = None;
    let mut star_pi: Option<usize> = None;

    while ti < n {
        if pi < m {
            let pc = pat[pi];
            if pc == '%' {
                star_pi = Some(pi);
                star_ti = Some(ti);
                pi += 1;
                continue;
            }
            if pc == '_' {
                pi += 1;
                ti += 1;
                continue;
            }
            // Escape support: `\X` matches a literal X for X in {%, _, \}.
            // Outside that set the backslash is itself literal (matches
            // SQLite's loose default).
            let (effective_pat, advance) = if pc == '\\' && pi + 1 < m {
                let nxt = pat[pi + 1];
                if nxt == '%' || nxt == '_' || nxt == '\\' {
                    (nxt, 2)
                } else {
                    (pc, 1)
                }
            } else {
                (pc, 1)
            };
            if char_eq(text[ti], effective_pat, case_insensitive) {
                pi += advance;
                ti += 1;
                continue;
            }
        }
        // Mismatch (or pattern exhausted before text). If a backtrack point
        // exists, expand the last `%` to absorb one more char and retry.
        if let (Some(spi), Some(sti)) = (star_pi, star_ti) {
            pi = spi + 1;
            star_ti = Some(sti + 1);
            ti = sti + 1;
        } else {
            return false;
        }
    }
    // Text exhausted; pattern must be done (or all that's left is `%`).
    while pi < m && pat[pi] == '%' {
        pi += 1;
    }
    pi == m
}

fn char_eq(a: char, b: char, case_insensitive: bool) -> bool {
    if !case_insensitive {
        return a == b;
    }
    if a.is_ascii() && b.is_ascii() {
        a.eq_ignore_ascii_case(&b)
    } else {
        a == b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn like_simple_literal() {
        assert!(like_match("apple", "apple", true));
        assert!(!like_match("apple", "apples", true));
    }

    #[test]
    fn like_percent_wildcard() {
        assert!(like_match("apple", "a%", true));
        assert!(like_match("apple", "%le", true));
        assert!(like_match("apple", "%pp%", true));
        assert!(!like_match("banana", "a%", true));
    }

    #[test]
    fn like_underscore_wildcard() {
        assert!(like_match("abc", "a_c", true));
        assert!(!like_match("abbc", "a_c", true));
    }

    #[test]
    fn like_case_insensitive_default() {
        assert!(like_match("Apple", "a%", true));
        assert!(like_match("APPLE", "%le", true));
        assert!(
            !like_match("Apple", "a%", false),
            "case-sensitive should fail"
        );
    }

    #[test]
    fn like_escape_percent_literal() {
        // pattern `100\%` should match literal "100%"
        assert!(like_match("100%", "100\\%", true));
        assert!(!like_match("100x", "100\\%", true));
    }

    #[test]
    fn like_no_pathological_recursion() {
        // The classic "exponential naive matcher" stress case.
        let text = "a".repeat(40);
        let pat = "a%a%a%a%a%a%a%a%b";
        // Should return false in linear time; if we recurse we'd stack-OOM
        // or hang; this test is mostly a smoke test.
        assert!(!like_match(&text, pat, true));
    }

    #[test]
    fn distinct_key_real_distinguishes_from_int() {
        let a = DistinctKey::from_value(&Value::Integer(1));
        let b = DistinctKey::from_value(&Value::Real(1.0));
        assert_ne!(a, b, "Integer(1) vs Real(1.0) must hash differently");
    }

    #[test]
    fn count_star_includes_nulls() {
        let call = AggregateCall {
            func: AggregateFn::Count,
            arg: AggregateArg::Star,
            distinct: false,
        };
        let mut s = AggState::new(&call);
        s.update(&Value::Null).unwrap();
        s.update(&Value::Integer(7)).unwrap();
        s.update(&Value::Null).unwrap();
        assert_eq!(s.finalize(), Value::Integer(3));
    }

    #[test]
    fn count_col_skips_nulls() {
        let call = AggregateCall {
            func: AggregateFn::Count,
            arg: AggregateArg::Column("x".into()),
            distinct: false,
        };
        let mut s = AggState::new(&call);
        s.update(&Value::Null).unwrap();
        s.update(&Value::Integer(7)).unwrap();
        s.update(&Value::Null).unwrap();
        assert_eq!(s.finalize(), Value::Integer(1));
    }

    #[test]
    fn count_distinct_dedupes() {
        let call = AggregateCall {
            func: AggregateFn::Count,
            arg: AggregateArg::Column("x".into()),
            distinct: true,
        };
        let mut s = AggState::new(&call);
        for v in [1, 1, 2, 2, 3, 3] {
            s.update(&Value::Integer(v)).unwrap();
        }
        s.update(&Value::Null).unwrap();
        assert_eq!(s.finalize(), Value::Integer(3));
    }

    #[test]
    fn sum_int_stays_int_until_real() {
        let call = AggregateCall {
            func: AggregateFn::Sum,
            arg: AggregateArg::Column("x".into()),
            distinct: false,
        };
        let mut s = AggState::new(&call);
        s.update(&Value::Integer(2)).unwrap();
        s.update(&Value::Integer(3)).unwrap();
        assert_eq!(s.finalize(), Value::Integer(5));

        s.update(&Value::Real(0.5)).unwrap();
        match s.finalize() {
            Value::Real(r) => assert!((r - 5.5).abs() < 1e-9),
            v => panic!("expected Real, got {:?}", v),
        }
    }

    #[test]
    fn sum_all_null_is_null() {
        let call = AggregateCall {
            func: AggregateFn::Sum,
            arg: AggregateArg::Column("x".into()),
            distinct: false,
        };
        let mut s = AggState::new(&call);
        s.update(&Value::Null).unwrap();
        s.update(&Value::Null).unwrap();
        assert_eq!(s.finalize(), Value::Null);
    }

    #[test]
    fn avg_always_real() {
        let call = AggregateCall {
            func: AggregateFn::Avg,
            arg: AggregateArg::Column("x".into()),
            distinct: false,
        };
        let mut s = AggState::new(&call);
        s.update(&Value::Integer(2)).unwrap();
        s.update(&Value::Integer(4)).unwrap();
        match s.finalize() {
            Value::Real(r) => assert!((r - 3.0).abs() < 1e-9),
            v => panic!("expected Real, got {:?}", v),
        }
    }

    #[test]
    fn min_max_skip_nulls() {
        let mk = |f| AggregateCall {
            func: f,
            arg: AggregateArg::Column("x".into()),
            distinct: false,
        };
        let mut mn = AggState::new(&mk(AggregateFn::Min));
        let mut mx = AggState::new(&mk(AggregateFn::Max));
        for v in [
            Value::Null,
            Value::Integer(7),
            Value::Integer(3),
            Value::Integer(9),
            Value::Null,
        ] {
            mn.update(&v).unwrap();
            mx.update(&v).unwrap();
        }
        assert_eq!(mn.finalize(), Value::Integer(3));
        assert_eq!(mx.finalize(), Value::Integer(9));
    }
}
