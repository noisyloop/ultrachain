# Bridge web app (sketch)

Users shouldn't need the command line to bridge. Hyperlane's open-source
[Warp UI template](https://github.com/hyperlane-xyz/hyperlane-warp-ui-template)
is a Next.js app for exactly this; it needs to know about Ultrachain and the
USDC route.

1. After the `warp` stage, run `scripts/gen-ui-config.py`. It writes:
   - `chains/ultrachain/metadata.yaml` and `addresses.yaml` (Ultrachain's
     registry entry; fill in the TODOs: native token, explorer, block time);
   - `warp-route-config.yaml` (the USDC route in Hyperlane's WarpCoreConfig
     format, including the synthetic mint address derived from the router).
2. Fork the Warp UI template, add the chain entry to its chain config and the
   route to its warp route config (see that repo's README for the exact
   files, which change between releases), then deploy it, e.g. to Vercel.
3. Optional, for Hyperlane's explorer and other apps: open a PR to
   [hyperlane-registry](https://github.com/hyperlane-xyz/hyperlane-registry)
   adding `chains/ultrachain/` and the route under
   `deployments/warp_routes/USDC/`.

The app relays through whoever runs a relayer for this route, which is
**your** relayer: if it's down, transfers sit undelivered until it's back.
