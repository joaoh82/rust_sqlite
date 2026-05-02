# Build + serve the SQLRite WASM browser demo.
#
# Targets:
#   make build       — build the wasm-pack output into ./pkg/
#   make serve       — serve static demo on http://localhost:8080 (no Ask)
#   make ask-demo    — serve via node server.mjs (static + Ask proxy)
#   make             — build + serve (no Ask)
#   make clean       — remove the built pkg/
#
# The plain `serve` target uses Python's stdlib http.server — fine
# for the SQL console part, but the Ask demo needs a backend that
# proxies POST /api/llm/complete to Anthropic. Use `make ask-demo`
# for that flow.

.PHONY: all build serve ask-demo clean

PORT ?= 8080

all: build serve

# Point at the sdk/wasm crate via a relative path. `wasm-pack build
# --out-dir` writes the generated JS + wasm + .d.ts files straight
# into `examples/wasm/pkg/`, which `index.html` imports directly.
build:
	cd ../../sdk/wasm && wasm-pack build --target web --release --out-dir ../../examples/wasm/pkg

# Simple static server so the browser can fetch the wasm file via
# HTTP (file:// loads block WebAssembly for security reasons). Uses
# Python because it's on every macOS/Linux install; any static
# server works (miniserve, `npx serve`, etc.). Note: the Ask
# section in the demo will report "couldn't reach proxy" because
# python's http.server doesn't proxy. Use `make ask-demo` for that.
serve:
	@echo "Open http://localhost:$(PORT)/ in a browser"
	python3 -m http.server $(PORT)

# Static-server + Ask proxy in one process. Requires Node 18+
# (built-in fetch) and ANTHROPIC_API_KEY in the env. Zero npm
# dependencies — the server is ~70 LOC in server.mjs using only
# `node:http` and `node:fs`.
#
# After `make build`:
#
#     export ANTHROPIC_API_KEY=sk-ant-…
#     make ask-demo
#
# Then open http://localhost:8080/ and use the "Ask" section.
ask-demo:
	@echo "SQLRite WASM demo + Ask proxy:  http://localhost:$(PORT)/"
	PORT=$(PORT) node server.mjs

clean:
	rm -rf pkg
