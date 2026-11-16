"""Minimal JSON-RPC helper over free public endpoints (no API key)."""
import time
import requests

ENDPOINTS = [
    "https://gateway.tenderly.co/public/mainnet",
    "https://rpc.mevblocker.io",
]
_session = requests.Session()


def call(method, params, retries=6):
    last = None
    for attempt in range(retries):
        url = ENDPOINTS[attempt % len(ENDPOINTS)]
        try:
            r = _session.post(
                url,
                json={"jsonrpc": "2.0", "id": 1, "method": method, "params": params},
                timeout=60,
            )
            j = r.json()
            if "result" in j:
                return j["result"]
            last = j.get("error")
        except Exception as e:  # network / decode errors: retry on the other endpoint
            last = e
        time.sleep(0.5 * (attempt + 1))
    raise RuntimeError(f"{method} failed: {last}")


def get_logs(address, topics, start, end, step=50_000):
    """Fetch logs over [start, end], halving the range when a provider rejects it."""
    out = []
    lo = start
    while lo <= end:
        hi = min(lo + step - 1, end)
        try:
            out += call(
                "eth_getLogs",
                [{"address": address, "topics": topics, "fromBlock": hex(lo), "toBlock": hex(hi)}],
                retries=3,
            )
            lo = hi + 1
        except RuntimeError:
            if step <= 50:
                raise
            step //= 2
    return out
