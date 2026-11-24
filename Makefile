.PHONY: ci-minimum-supported-rust-version test-coverage test-benchmarks

ci-minimum-supported-rust-version:
	bin/make-ci-minimum-supported-rust-version

test-coverage:
	bin/test-coverage

test-benchmarks:
	bin/test-benchmarks
