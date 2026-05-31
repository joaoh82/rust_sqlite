# Convenience targets for the SQLRite Go edge/IoT collector.
#
# Everything depends on the engine's C library. `make lib` builds it
# from the repo root; the other targets assume it exists.

REPO_ROOT := ../..

.PHONY: lib build test vet run loadtest bench docker clean

## lib: build the SQLRite cdylib (libsqlrite_c) the cgo driver links.
lib:
	cd $(REPO_ROOT) && cargo build --release -p sqlrite-ffi

## build: compile the collector + loadgen binaries.
build: lib
	go build -o bin/collector ./cmd/collector
	go build -o bin/loadgen   ./cmd/loadgen

## test: run the Go test suite (needs lib).
test: lib
	go test ./... -count=1

## vet: type-check all packages and tests.
vet: lib
	go vet ./...

## run: start the collector on :8080 with a local DB file.
run: build
	./bin/collector -db events.sqlrite -addr :8080

## loadtest: fire concurrent producers at a running collector.
loadtest: build
	./bin/loadgen -target http://localhost:8080 -workers 64 -duration 30s

## bench: in-process throughput matrix (concurrent vs serialized, ±index).
bench: build
	./bin/loadgen -bench -workers 32 -duration 30s

## docker: build the container image (context = repo root).
docker:
	cd $(REPO_ROOT) && docker build -f examples/go-collector/Dockerfile -t sqlrite-collector .

## clean: remove build artifacts and local DB files.
clean:
	rm -rf bin
	rm -f events.sqlrite events.sqlrite-wal
