SHELL := pwsh.exe
.SHELLFLAGS := -NoLogo -Command

CARGO_TARGET_DIR := G:/tmp/pdf_splitter_build
BIN := pdf-splitter

.PHONY: help check build release run run-cli clean

help:
	@Write-Host "Targets:"
	@Write-Host "  make check                 Run cargo check"
	@Write-Host "  make build                 Build debug binary"
	@Write-Host "  make release               Build release binary"
	@Write-Host "  make run                   Launch GUI"
	@Write-Host "  make run-cli ARGS='...'    Run CLI mode with custom args"
	@Write-Host "  make clean                 Remove temp target dir"

check:
	$$env:CARGO_TARGET_DIR='$(CARGO_TARGET_DIR)'; cargo check

build:
	$$env:CARGO_TARGET_DIR='$(CARGO_TARGET_DIR)'; cargo build

release:
	$$env:CARGO_TARGET_DIR='$(CARGO_TARGET_DIR)'; cargo build --release

run:
	$$env:CARGO_TARGET_DIR='$(CARGO_TARGET_DIR)'; cargo run

run-cli:
	$$env:CARGO_TARGET_DIR='$(CARGO_TARGET_DIR)'; cargo run -- $(ARGS)

clean:
	if (Test-Path '$(CARGO_TARGET_DIR)') { Remove-Item -LiteralPath '$(CARGO_TARGET_DIR)' -Recurse -Force }
