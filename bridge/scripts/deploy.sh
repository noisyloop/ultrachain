#!/usr/bin/env bash
# Hyperlane deployment for Ultrachain <-> Solana mainnet, one stage at a time.
#
#   bridge/scripts/deploy.sh <stage>
#
# Stages, in order (see bridge/README.md for what each one does):
#   build            clone hyperlane-monorepo at a pinned commit, build programs + client
#   core             deploy Mailbox, ISM, IGP, ValidatorAnnounce to Ultrachain
#   ism-ultrachain   configure Ultrachain's default ISM to trust Solana's validators
#   warp             deploy the USDC warp route on both chains and enroll routers
#   ism-solana       deploy the Solana-side composite ISM (your validators +
#                    24h rate limit + pause switch) and attach it to the route
#   agent-config     write bridge/agents/agent-config.json for the validator/relayer
#   test-transfer    send a small USDC transfer Solana -> Ultrachain
#
# Operations, any time after deploy:
#   pause | unpause  stop / resume Ultrachain -> Solana releases
#   check-supply     verify Ultrachain synthetic supply <= USDC locked on Solana
#
# Required env: DEPLOYER_KEYPAIR (path to a Solana JSON keypair funded on both
# chains). Optional: HYPERLANE_COMMIT, SOLANA_RPC_URL, SOLANA_INDEX_FROM.
set -euo pipefail

BRIDGE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENV_DIR="$BRIDGE_DIR/environments"
ENV_NAME="mainnet"
REGISTRY="$BRIDGE_DIR/registry"
WARP_NAME="USDC-solanamainnet-ultrachain"
WARP_DIR="$ENV_DIR/$ENV_NAME/warp-routes/$WARP_NAME"

ULTRACHAIN_DOMAIN=1970041970
SOLANA_DOMAIN=1399811149
SOLANA_MAILBOX="E588QtVUvresuXq2KoNEwAmoifCzYGpRBdHByN9KQMbi"
SOLANA_ISM_DIR="$ENV_DIR/$ENV_NAME/composite-ism/solanamainnet"
SOLANA_ISM_CONFIG="$SOLANA_ISM_DIR/ism-config.json"

# Commit the configs in this directory were checked against.
HYPERLANE_COMMIT="${HYPERLANE_COMMIT:-18c39da556775e9ff8f86f5467f8d27c12516ba4}"
MONOREPO="${MONOREPO:-$BRIDGE_DIR/.build/hyperlane-monorepo}"
SO_DIR="$MONOREPO/rust/sealevel/target/deploy"
CLIENT="$MONOREPO/rust/sealevel/target/release/hyperlane-sealevel-client"

die() { echo "error: $*" >&2; exit 1; }
log() { echo "==> $*" >&2; }

# Fail early if a file still has placeholders the stage depends on.
require_filled() {
    for f in "$@"; do
        if grep -n "REPLACE_ME" "$f" >&2; then
            die "fill in the REPLACE_ME values in $f first"
        fi
    done
}

rpc_for() {
    python3 - "$REGISTRY/chains/metadata.yaml" "$1" <<'EOF'
import sys, re
# Tiny parser for this file's fixed layout; avoids a PyYAML dependency.
path, chain = sys.argv[1], sys.argv[2]
cur = None
for line in open(path):
    m = re.match(r"^(\w+):\s*$", line)
    if m:
        cur = m.group(1)
    m = re.match(r"^\s+- http:\s*(\S+)", line)
    if m and cur == chain:
        print(m.group(1)); break
EOF
}

json_get() { python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))[sys.argv[2]])' "$1" "$2"; }

solana_router() {
    python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["solanamainnet"][sys.argv[2]])' \
        "$WARP_DIR/program-ids.json" "$1"
}

# Flip the pausable node in the deployed ISM config and push it on-chain.
set_paused() {
    [[ -f "$SOLANA_ISM_CONFIG" ]] || die "run the 'ism-solana' stage first"
    python3 - "$SOLANA_ISM_CONFIG" "$1" <<'EOF'
import json, sys
path, paused = sys.argv[1], sys.argv[2] == "true"
cfg = json.load(open(path))
nodes = [n for n in cfg["sub_isms"] if n["type"] == "pausable"]
assert len(nodes) == 1, "expected exactly one pausable node"
nodes[0]["paused"] = paused
json.dump(cfg, open(path, "w"), indent=2)
EOF
    client -u "$SOLANA_RPC" composite-ism update \
        --program-id "$(json_get "$SOLANA_ISM_DIR/program-ids.json" program_id)" \
        --config-file "$SOLANA_ISM_CONFIG"
}

client() {
    [[ -x "$CLIENT" ]] || die "client not built; run the 'build' stage"
    [[ -n "${DEPLOYER_KEYPAIR:-}" ]] || die "set DEPLOYER_KEYPAIR to a Solana JSON keypair path"
    "$CLIENT" -k "$DEPLOYER_KEYPAIR" "$@"
}

ULTRACHAIN_RPC="$(rpc_for ultrachain)"
SOLANA_RPC="${SOLANA_RPC_URL:-$(rpc_for solanamainnet)}"

stage="${1:-}"
case "$stage" in
build)
    if [[ ! -d "$MONOREPO/.git" ]]; then
        git clone https://github.com/hyperlane-xyz/hyperlane-monorepo "$MONOREPO"
    fi
    git -C "$MONOREPO" fetch --depth 1 origin "$HYPERLANE_COMMIT"
    git -C "$MONOREPO" checkout --detach "$HYPERLANE_COMMIT"
    log "building sealevel programs (installs the pinned Solana CLI toolchain)"
    (cd "$MONOREPO/rust/sealevel/programs" && ./build-programs.sh all)
    log "building hyperlane-sealevel-client"
    (cd "$MONOREPO/rust/sealevel" && cargo build --release -p hyperlane-sealevel-client)
    ls "$SO_DIR"/*.so
    ;;

core)
    require_filled "$REGISTRY/chains/metadata.yaml"
    client -u "$ULTRACHAIN_RPC" core deploy \
        --local-domain "$ULTRACHAIN_DOMAIN" \
        --environment "$ENV_NAME" --environments-dir "$ENV_DIR" \
        --chain ultrachain \
        --remote-domains "$SOLANA_DOMAIN" \
        --built-so-dir "$SO_DIR"
    cat "$ENV_DIR/$ENV_NAME/ultrachain/core/program-ids.json"
    ;;

ism-ultrachain)
    ism="$(json_get "$ENV_DIR/$ENV_NAME/ultrachain/core/program-ids.json" multisig_ism_message_id)"
    client -u "$ULTRACHAIN_RPC" multisig-ism-message-id configure \
        --program-id "$ism" \
        --multisig-config-file "$BRIDGE_DIR/config/ultrachain-multisig-config.json" \
        --registry "$REGISTRY"
    client -u "$ULTRACHAIN_RPC" multisig-ism-message-id query --program-id "$ism" --domains "$SOLANA_DOMAIN"
    ;;

warp)
    require_filled "$WARP_DIR/token-config.json"
    # The first run leaves the Solana side on the Solana mailbox's default ISM,
    # which has no validators for Ultrachain, so nothing can be released on
    # Solana until 'ism-solana' attaches our ISM. Re-running is idempotent.
    client -u "$SOLANA_RPC" warp-route deploy \
        --environment "$ENV_NAME" --environments-dir "$ENV_DIR" \
        --built-so-dir "$SO_DIR" \
        --warp-route-name "$WARP_NAME" \
        --token-config-file "$WARP_DIR/token-config.json" \
        --registry "$REGISTRY" \
        --ata-payer-funding-amount 10000000
    cat "$WARP_DIR/program-ids.json"
    ;;

ism-solana)
    template="$BRIDGE_DIR/config/solana-ism.template.json"
    require_filled "$template"
    [[ -f "$WARP_DIR/program-ids.json" ]] || die "run the 'warp' stage first"
    mkdir -p "$SOLANA_ISM_DIR"
    # The rate limit is bound to our Solana router, so it only exists after 'warp'.
    python3 - "$template" "$SOLANA_ISM_CONFIG" "$SOLANA_MAILBOX" "$(solana_router hex)" <<'EOF'
import json, sys
import os
src, dst, mailbox, recipient = sys.argv[1:]
cfg = json.load(open(src))
cfg.pop("_comment", None)
# Keep the current pause state: re-running this stage must never unpause.
paused = None
if os.path.exists(dst):
    paused = next(n["paused"] for n in json.load(open(dst))["sub_isms"] if n["type"] == "pausable")
for node in cfg["sub_isms"]:
    if node["type"] == "rateLimited":
        node["mailbox"], node["recipient"] = mailbox, recipient
    if node["type"] == "pausable" and paused is not None:
        node["paused"] = paused
json.dump(cfg, open(dst, "w"), indent=2)
EOF
    if [[ -f "$SOLANA_ISM_DIR/program-ids.json" ]]; then
        ism="$(json_get "$SOLANA_ISM_DIR/program-ids.json" program_id)"
        log "ISM already deployed ($ism); updating its config"
        client -u "$SOLANA_RPC" composite-ism update --program-id "$ism" --config-file "$SOLANA_ISM_CONFIG"
    else
        client -u "$SOLANA_RPC" composite-ism deploy \
            --environment "$ENV_NAME" --environments-dir "$ENV_DIR" \
            --built-so-dir "$SO_DIR" \
            --chain solanamainnet --local-domain "$SOLANA_DOMAIN" \
            --config-file "$SOLANA_ISM_CONFIG"
        ism="$(json_get "$SOLANA_ISM_DIR/program-ids.json" program_id)"
    fi
    client -u "$SOLANA_RPC" token set-interchain-security-module \
        --program-id "$(solana_router base58)" --ism "$ism"
    # Record it in the token config too, or a later 'warp' re-run would reset
    # the route back to the mailbox default ISM.
    python3 - "$WARP_DIR/token-config.json" "$ism" <<'EOF'
import json, sys
path, ism = sys.argv[1:]
cfg = json.load(open(path))
cfg["solanamainnet"]["interchainSecurityModule"] = ism
open(path, "w").write(json.dumps(cfg, indent=2) + "\n")
EOF
    client -u "$SOLANA_RPC" composite-ism read --program-id "$ism"
    ;;

agent-config)
    core="$ENV_DIR/$ENV_NAME/ultrachain/core/program-ids.json"
    [[ -f "$core" ]] || die "run the 'core' stage first"
    [[ -n "${SOLANA_INDEX_FROM:-}" ]] || die \
        "set SOLANA_INDEX_FROM to Solana's current mailbox nonce (see README) so the relayer doesn't index all of Solana's history"
    python3 "$BRIDGE_DIR/scripts/gen-agent-config.py" \
        --core "$core" --ultrachain-rpc "$ULTRACHAIN_RPC" --solana-rpc "$SOLANA_RPC" \
        --solana-index-from "$SOLANA_INDEX_FROM" \
        --warp "$WARP_DIR/program-ids.json" \
        --out-dir "$BRIDGE_DIR/agents"
    ;;

test-transfer)
    # usage: deploy.sh test-transfer <sender-keypair.json> <amount-base-units> <ultrachain-recipient-pubkey>
    [[ $# -eq 4 ]] || die "usage: deploy.sh test-transfer <sender-keypair.json> <amount-base-units> <recipient>"
    client -u "$SOLANA_RPC" token transfer-remote "$2" "$3" "$ULTRACHAIN_DOMAIN" "$4" collateral \
        --program-id "$(solana_router base58)"
    ;;

pause)
    # Any config update also resets the rate limiter's on-chain state.
    set_paused true
    log "paused: Ultrachain -> Solana messages will be rejected on Solana"
    ;;

unpause)
    set_paused false
    log "unpaused"
    ;;

check-supply)
    python3 "$BRIDGE_DIR/scripts/check-supply.py" --warp "$WARP_DIR/program-ids.json" \
        --solana-rpc "$SOLANA_RPC" --ultrachain-rpc "$ULTRACHAIN_RPC"
    ;;

*)
    sed -n '2,21p' "$0" | sed 's/^# \{0,1\}//'
    exit 1
    ;;
esac
