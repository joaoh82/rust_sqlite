SHELL:=/bin/bash

# COLORS
GREEN  := $(shell tput -Txterm setaf 2)
YELLOW := $(shell tput -Txterm setaf 3)
WHITE  := $(shell tput -Txterm setaf 7)
RESET  := $(shell tput -Txterm sgr0)

VERSION=production
COMMIT=$(shell git rev-parse HEAD)
GITDIRTY=$(shell git diff --quiet || echo 'dirty')

GIT_BRANCH := $(shell git rev-parse --abbrev-ref HEAD)

TARGET_MAX_CHAR_NUM=25
## Show help
help:
	@echo ''
	@echo 'Usage:'
	@echo '  ${YELLOW}make${RESET} ${GREEN}<target>${RESET}'
	@echo ''
	@echo 'Targets:'
	@awk '/^[a-zA-Z\-\_0-9]+:/ { \
		helpMessage = match(lastLine, /^## (.*)/); \
		if (helpMessage) { \
			helpCommand = substr($$1, 0, index($$1, ":")-1); \
			helpMessage = substr(lastLine, RSTART + 3, RLENGTH); \
			printf "  ${YELLOW}%-$(TARGET_MAX_CHAR_NUM)s${RESET} ${GREEN}%s${RESET}\n", helpCommand, helpMessage; \
		} \
	} \
	{ lastLine = $$0 }' $(MAKEFILE_LIST)

.PHONY: code-lines
## Count number of lines of code in the repository
code-lines:
	@echo '${GREEN}Counting${RESET} ${YELLOW}number of lines ${RESET} of code'
	@git ls-files | xargs wc -l

.PHONY: generate-docs
## Generate Rust Docs based on comments
generate-docs:
	@echo '${GREEN}Generating${RESET} ${YELLOW}documentation ${RESET} for SQLRite'
	@cargo doc --open

.PHONY: bench
## Run the benchmark suite (SQLR-4 / SQLR-16) — SQLRite vs SQLite (lean)
bench:
	@echo '${GREEN}Running${RESET} ${YELLOW}benchmark suite${RESET} (SQLRite vs SQLite, lean profile)'
	@./benchmarks/scripts/run.sh

.PHONY: bench-duckdb
## Run the benchmark suite with DuckDB enabled (Group B only — W7/W8/W9)
bench-duckdb:
	@echo '${GREEN}Running${RESET} ${YELLOW}benchmark suite${RESET} (with DuckDB driver, Group B only)'
	@FEATURES=duckdb ./benchmarks/scripts/run.sh
