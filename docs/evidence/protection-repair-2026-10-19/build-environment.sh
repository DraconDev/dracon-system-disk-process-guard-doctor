# Source this environment snapshot before reproducing the recorded Cargo gates.
# These are installed Nix store inputs, not downloaded files or ambient secrets.
# For another machine, use the parent workspace's locked `nix develop` shell.
export OPENSSL_DIR=/nix/store/cqkz127l4l98xgp7p6y1j2cqvyx0w3jl-openssl-3.6.2-dev
export OPENSSL_INCLUDE_DIR="$OPENSSL_DIR/include"
export OPENSSL_LIB_DIR=/nix/store/by19gg2pqp2b1gqphdf6bwk2g3hgak79-openssl-3.6.2/lib
# Do not depend on the caller's graphical-desktop pkg-config search path.
export PKG_CONFIG_PATH="$OPENSSL_DIR/lib/pkgconfig"
export CARGO_BUILD_JOBS=2
if ! test -f "$OPENSSL_INCLUDE_DIR/openssl/ssl.h" || ! test -f "$OPENSSL_LIB_DIR/libssl.so"; then
    printf '%s\n' 'Recorded OpenSSL store inputs are absent; enter the locked workspace nix develop shell instead.' >&2
    return 1
fi
