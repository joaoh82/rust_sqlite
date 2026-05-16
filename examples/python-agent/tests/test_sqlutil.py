from sqlrite_agent.sqlutil import q, vec_literal


def test_q_escapes_single_quotes():
    assert q("it's") == "'it''s'"


def test_q_handles_none_and_numbers():
    assert q(None) == "NULL"
    assert q(42) == "42"
    assert q(True) == "1"
    assert q(False) == "0"


def test_q_vectors_use_brackets():
    assert q([1.0, 2.0]) == "[1.000000, 2.000000]"


def test_vec_literal_rounds_floats():
    out = vec_literal([0.1, -0.5, 1.234567])
    assert out == "[0.100000, -0.500000, 1.234567]"


def test_q_rejects_unknown_types():
    import pytest

    with pytest.raises(TypeError):
        q(object())
