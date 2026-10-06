name := "cosmic-audio-splitter"
appid := "io.github.okrroni.splitter"
rootdir := ""
prefix := "/usr"
cargo-target-dir := env("CARGO_TARGET_DIR", "target")

default: build-release

build-debug:
    cargo build --locked

build-release:
    cargo build --release --locked

check:
    cargo fmt --all -- --check
    cargo clippy --all-targets --locked -- -D warnings
    cargo test --all-targets --locked

run:
    env RUST_BACKTRACE=1 cargo run --locked

install:
    install -Dm0755 {{cargo-target-dir}}/release/{{name}} {{rootdir}}{{prefix}}/bin/{{name}}
    install -Dm0644 resources/{{appid}}.desktop {{rootdir}}{{prefix}}/share/applications/{{appid}}.desktop
    install -Dm0644 resources/{{appid}}.metainfo.xml {{rootdir}}{{prefix}}/share/metainfo/{{appid}}.metainfo.xml
    install -Dm0644 resources/icons/hicolor/scalable/apps/{{appid}}.svg {{rootdir}}{{prefix}}/share/icons/hicolor/scalable/apps/{{appid}}.svg

uninstall:
    rm -f {{rootdir}}{{prefix}}/bin/{{name}}
    rm -f {{rootdir}}{{prefix}}/share/applications/{{appid}}.desktop
    rm -f {{rootdir}}{{prefix}}/share/metainfo/{{appid}}.metainfo.xml
    rm -f {{rootdir}}{{prefix}}/share/icons/hicolor/scalable/apps/{{appid}}.svg
