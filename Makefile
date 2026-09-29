.PHONY: build test lint clean demo

build:
	cargo build --release

test:
	cargo test

lint:
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings

clean:
	cargo clean
