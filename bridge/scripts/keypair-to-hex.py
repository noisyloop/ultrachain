#!/usr/bin/env python3
"""Convert a Solana JSON keypair file to the 0x-hex form Hyperlane agents
expect for sealevel signers (all 64 bytes: secret key then public key)."""
import json
import sys

if len(sys.argv) != 2:
    sys.exit("usage: keypair-to-hex.py <keypair.json>")
key = bytes(json.load(open(sys.argv[1])))
if len(key) != 64:
    sys.exit(f"expected a 64-byte keypair, got {len(key)} bytes")
print("0x" + key.hex())
