.PHONY: build test lint clean demo server install

build:
	cargo build --release

install:
	cargo install --path .

server:
	./bvd

test:
	cargo test

lint:
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings

demo:
	./scripts/demo.sh

clean:
	cargo clean
