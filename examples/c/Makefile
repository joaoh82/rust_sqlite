# Build the SQLRite C sample.
#
# Run from this directory (`examples/c/`). `make` first builds the
# Rust cdylib via cargo, then compiles `hello.c` against its header
# and shared library. The resulting binary embeds an rpath pointing
# at the cargo target dir so `./hello` runs without any
# LD_LIBRARY_PATH / DYLD_* environment juggling.
#
# Targets:
#   make hello   — build the example
#   make run     — build + run
#   make clean   — remove the binary (cargo artifacts stay)

# Relative paths from `examples/c/` up to the workspace root. Kept
# literal (not computed via `git` / `cd … && pwd`) because `$(shell …)`
# with multi-command fallbacks is fiddly across make versions.
REPO_ROOT  := ../..
HEADER_DIR := $(REPO_ROOT)/sqlrite-ffi/include
LIB_DIR    := $(REPO_ROOT)/target/release
LIB_NAME   := sqlrite_c

CC      ?= cc
CFLAGS  ?= -Wall -Wextra -O2
LDFLAGS := -L$(LIB_DIR) -l$(LIB_NAME) -Wl,-rpath,$(abspath $(LIB_DIR))

.PHONY: all run clean lib

all: hello

# Build the Rust cdylib. Cargo handles up-to-date checks internally
# so this is cheap on incremental builds.
lib:
	cd $(REPO_ROOT) && cargo build --release -p sqlrite-ffi

hello: hello.c lib
	$(CC) $(CFLAGS) -I$(HEADER_DIR) hello.c -o hello $(LDFLAGS)

run: hello
	./hello

clean:
	rm -f hello
