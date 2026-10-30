#!/usr/bin/env python3
"""Check the bridge invariant: synthetic USDC on Ultrachain must never exceed
the USDC locked in the Solana escrow.

    check-supply.py [--warp program-ids.json] [--solana-rpc URL] [--ultrachain-rpc URL]

Exits 0 when backed, 2 when the synthetic supply exceeds the collateral (page
someone), 1 on errors. Supply can briefly be *below* collateral while a
Solana -> Ultrachain transfer is in flight; that is normal.

Stdlib only, so it runs anywhere a cron job or monitor can call it.
"""
import argparse
import hashlib
import json
import pathlib
import sys
import urllib.request

BRIDGE_DIR = pathlib.Path(__file__).resolve().parent.parent
DEFAULT_WARP = BRIDGE_DIR / "environments/mainnet/warp-routes/USDC-solanamainnet-ultrachain/program-ids.json"

# --- base58 -----------------------------------------------------------------

B58 = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"


def b58decode(s: str) -> bytes:
    n = 0
    for c in s:
        n = n * 58 + B58.index(c)
    raw = n.to_bytes((n.bit_length() + 7) // 8, "big") if n else b""
    pad = len(s) - len(s.lstrip("1"))
    return b"\x00" * pad + raw


def b58encode(b: bytes) -> str:
    n = int.from_bytes(b, "big")
    out = ""
    while n:
        n, r = divmod(n, 58)
        out = B58[r] + out
    pad = len(b) - len(b.lstrip(b"\x00"))
    return "1" * pad + out


# --- program derived addresses ------------------------------------------------

P = 2**255 - 19
D = (-121665 * pow(121666, P - 2, P)) % P


def on_curve(key: bytes) -> bool:
    """Whether 32 bytes decode to a valid ed25519 point (PDAs must not)."""
    y = int.from_bytes(key, "little") & ((1 << 255) - 1)
    sign = key[31] >> 7
    if y >= P:
        return False
    u = (y * y - 1) % P
    v = (D * y * y + 1) % P
    x2 = u * pow(v, P - 2, P) % P
    if x2 == 0:
        return sign == 0
    return pow(x2, (P - 1) // 2, P) == 1


def find_program_address(seeds: list[bytes], program_id: bytes) -> str:
    for bump in range(255, -1, -1):
        h = hashlib.sha256(b"".join(seeds) + bytes([bump]) + program_id + b"ProgramDerivedAddress").digest()
        if not on_curve(h):
            return b58encode(h)
    raise ValueError("no viable bump")


def escrow_address(collateral_program: str) -> str:
    return find_program_address([b"hyperlane_token", b"-", b"escrow"], b58decode(collateral_program))


def mint_address(synthetic_program: str) -> str:
    return find_program_address([b"hyperlane_token", b"-", b"mint"], b58decode(synthetic_program))


# --- RPC --------------------------------------------------------------------


def rpc(url: str, method: str, params: list) -> dict:
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode()
    req = urllib.request.Request(url, body, {"content-type": "application/json"})
    with urllib.request.urlopen(req, timeout=20) as resp:
        out = json.load(resp)
    if "error" in out:
        raise RuntimeError(f"{method}: {out['error']}")
    return out["result"]["value"]


def rpc_from_registry(chain: str) -> str:
    cur = None
    for line in (BRIDGE_DIR / "registry/chains/metadata.yaml").read_text().splitlines():
        if line and not line[0].isspace() and line.endswith(":"):
            cur = line[:-1]
        elif cur == chain and line.strip().startswith("- http:"):
            return line.split("http:", 1)[1].strip()
    raise SystemExit(f"no RPC for {chain} in registry")


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--warp", default=str(DEFAULT_WARP))
    ap.add_argument("--solana-rpc")
    ap.add_argument("--ultrachain-rpc")
    a = ap.parse_args()

    warp = json.load(open(a.warp))
    escrow = escrow_address(warp["solanamainnet"]["base58"])
    mint = mint_address(warp["ultrachain"]["base58"])
    sol_rpc = a.solana_rpc or rpc_from_registry("solanamainnet")
    ultra_rpc = a.ultrachain_rpc or rpc_from_registry("ultrachain")

    try:
        locked = int(rpc(sol_rpc, "getTokenAccountBalance", [escrow])["amount"])
        supply = int(rpc(ultra_rpc, "getTokenSupply", [mint])["amount"])
    except Exception as e:  # noqa: BLE001 - report any RPC failure the same way
        print(f"error: {e}", file=sys.stderr)
        return 1

    print(f"solana escrow {escrow}: {locked / 1e6:,.6f} USDC locked")
    print(f"ultrachain mint {mint}: {supply / 1e6:,.6f} USDC minted")
    if supply > locked:
        print(f"ALERT: synthetic supply exceeds collateral by {(supply - locked) / 1e6:,.6f} USDC", file=sys.stderr)
        return 2
    print("ok: fully backed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
