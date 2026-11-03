"""Thin RPC helpers: retrying provider rotation + multicall3 batching."""
import time
import logging
from typing import Any, Iterable
from web3 import Web3
from web3.exceptions import Web3RPCError
from eth_abi import decode as abi_decode

from .config import RPC_URLS, MULTICALL3, MULTICALL3_ABI

log = logging.getLogger("aero.rpc")


class Rpc:
    def __init__(self, urls=RPC_URLS):
        self.w3s = [Web3(Web3.HTTPProvider(u, request_kwargs={"timeout": 60})) for u in urls]
        self.i = 0

    @property
    def w3(self) -> Web3:
        return self.w3s[self.i]

    def rotate(self):
        self.i = (self.i + 1) % len(self.w3s)

    def call(self, fn, *args, attempts=8, **kw) -> Any:
        """Run fn(w3, *args) with retries, rotating providers on failure."""
        last = None
        for a in range(attempts):
            try:
                return fn(self.w3, *args, **kw)
            except Exception as e:  # noqa: BLE001
                last = e
                msg = str(e)
                log.debug("rpc error (%s): %s", self.w3.provider.endpoint_uri, msg[:160])
                if "execution reverted" in msg:
                    raise
                self.rotate()
                time.sleep(min(1.0 * (2 ** a), 20))
        raise RuntimeError(f"rpc failed after {attempts} attempts: {last}")

    def block_number(self) -> int:
        return self.call(lambda w3: w3.eth.block_number)

    def block_ts(self, n: int) -> int:
        return self.call(lambda w3: w3.eth.get_block(n)["timestamp"])

    def find_block_at(self, ts: int) -> int:
        """First block with timestamp >= ts (binary search; Base ~2s blocks)."""
        hi = self.block_number()
        hi_ts = self.block_ts(hi)
        lo = max(0, hi - int((hi_ts - ts) / 2) - 5000)
        while self.block_ts(lo) > ts:
            lo = max(0, lo - 50000)
        while lo < hi:
            mid = (lo + hi) // 2
            if self.block_ts(mid) < ts:
                lo = mid + 1
            else:
                hi = mid
        return lo

    def multicall(self, calls: list[tuple[str, bytes]], out_types: list[list[str]] | None = None,
                  batch: int = 300, block="latest") -> list[Any]:
        """calls = [(target, calldata)], returns list of decoded tuples (or None on failure)."""
        results: list[Any] = []
        mc = self.w3.eth.contract(address=Web3.to_checksum_address(MULTICALL3), abi=MULTICALL3_ABI)
        for s in range(0, len(calls), batch):
            chunk = calls[s:s + batch]
            def run(w3):
                c = w3.eth.contract(address=Web3.to_checksum_address(MULTICALL3), abi=MULTICALL3_ABI)
                return c.functions.aggregate3([(Web3.to_checksum_address(t), True, d) for t, d in chunk]).call(block_identifier=block)
            rets = self.call(run)
            for j, (ok, data) in enumerate(rets):
                if not ok or len(data) == 0:
                    results.append(None)
                    continue
                if out_types is None:
                    results.append(data)
                else:
                    types = out_types[s + j]
                    results.append(abi_decode(types, data))
        return results

    def get_logs(self, addresses: list[str], topics: list, from_block: int, to_block: int,
                 addr_chunk: int = 20, span: int = 10_000) -> list[dict]:
        out = []
        addrs = [Web3.to_checksum_address(a) for a in addresses]
        for a0 in range(0, len(addrs), addr_chunk):
            sub = addrs[a0:a0 + addr_chunk]
            f = from_block
            while f <= to_block:
                t = min(f + span - 1, to_block)
                logs = self.call(lambda w3: w3.eth.get_logs({
                    "address": sub, "topics": topics,
                    "fromBlock": f, "toBlock": t}))
                out.extend(logs)
                f = t + 1
        return out


def encode_call(w3: Web3, abi: list, fn: str, args: Iterable = ()) -> bytes:
    c = w3.eth.contract(abi=abi)
    return bytes.fromhex(c.encode_abi(abi_element_identifier=fn, args=list(args))[2:])
