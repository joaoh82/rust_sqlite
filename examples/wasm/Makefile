# Build + serve the SQLRite WASM browser demo.
#
# Targets:
#   make build    — build the wasm-pack output into ./pkg/
#   make serve    — serve the demo on http://localhost:8080
#   make          — build + serve
#   make clean    — remove the built pkg/

.PHONY: all build serve clean

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
# server works (miniserve, `npx serve`, etc.).
serve:
	@echo "Open http://localhost:$(PORT)/ in a browser"
	python3 -m http.server $(PORT)

clean:
	rm -rf pkg
