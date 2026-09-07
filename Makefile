.PHONY: fmt test lint check

fmt:
	cargo fmt --all

test:
	cargo test --all-targets

lint:
	cargo clippy --all-targets --all-features -- -D warnings

check: fmt test lint
