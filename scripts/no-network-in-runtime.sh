#!/usr/bin/env sh
set -eu

if [ ! -d crates ] && [ ! -d src ]; then
  echo "No src or crates directory found."
  exit 0
fi

# Scoped source scan: root binary sources under src/ plus crate implementation
# sources under crates/*/src. Tests, examples, manifests, and dependency graphs
# are outside this textual guard.
tmp_files="$(mktemp)"
trap 'rm -f "$tmp_files"' EXIT HUP INT TERM

# The web-share listener is the explicit TCP/WebSocket boundary; this guard
# protects the rest of the runtime from accidental network/browser deps.
{
  if [ -d src ]; then
    find src -type f -name '*.rs' 2>/dev/null
  fi
  if [ -d crates ]; then
    find crates -path '*/src/*' -type f -name '*.rs' 2>/dev/null
  fi
} \
  | grep -v '/target/' \
  | grep -v '^crates/rmux-server/src/web/' \
  | grep -Ev '(^|/)(tests?|[^/]*_tests)(/|\.rs$)' \
  >"$tmp_files" || true

if [ ! -s "$tmp_files" ]; then
  echo "No scoped runtime source files found."
  exit 0
fi

network_pattern='(^|[^[:alnum:]_])(std::net[[:space:]]*::[[:space:]]*(\{[^}]*((TcpListener|TcpStream|UdpSocket|ToSocketAddrs))[^}]*\}|TcpListener|TcpStream|UdpSocket|ToSocketAddrs)|tokio::net[[:space:]]*::[[:space:]]*(\{[^}]*((TcpListener|TcpStream|UdpSocket))[^}]*\}|TcpListener|TcpStream|UdpSocket)|mio::net[[:space:]]*::[[:space:]]*(\{[^}]*((TcpListener|TcpStream|UdpSocket))[^}]*\}|TcpListener|TcpStream|UdpSocket)|TcpListener|TcpStream|UdpSocket|ToSocketAddrs|socket2[[:space:]]*::|hyper[[:space:]]*::|reqwest[[:space:]]*::|ureq[[:space:]]*::|isahc[[:space:]]*::|surf[[:space:]]*::|tungstenite|tokio_tungstenite|WebSocket|web_sys[[:space:]]*::|wasm_bindgen|gloo_net|axum[[:space:]]*::|warp[[:space:]]*::|actix_web[[:space:]]*::|rocket[[:space:]]*::|tonic[[:space:]]*::|quinn[[:space:]]*::)'

matches="$(
  while IFS= read -r file; do
    grep -In -E "$network_pattern" "$file" || true
  done <"$tmp_files"
)"
if [ -n "$matches" ]; then
  echo "$matches"
  echo "Possible network, WebSocket, or browser runtime references found in scoped source scan." >&2
  exit 1
fi

echo "No network, WebSocket, or browser runtime references found in scoped src plus crates/*/src source scan."
