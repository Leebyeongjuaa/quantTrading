"""Aero (merged Aerodrome/Velodrome, live 2026-10-22) contract addresses on Base.

Sources: Dromos-Labs/metadex-public deployment-addresses/{root,leaf,gaugestack}-base.json
and https://app.aero.xyz/docs/security/
"""

CHAIN_ID = 8453

# Launch of the new Aero system (allocation opened 2026-10-21 20:00 EDT = 2026-10-22 00:00 UTC)
LAUNCH_TS = 1792627200

RPC_URLS = [
    "https://base-rpc.publicnode.com",
    "https://mainnet.base.org",
    "https://base.drpc.org",
]

MULTICALL3 = "0xcA11bde05977b3631167028862bE2a173976CA11"

VOTER = "0x49d7d4f17Ec56Ef8fA04E464AF5D1A4942CC31e6"          # root voter (Base)
LEAF_VOTER = "0x382409fad8503FbCdbB27A4211551fa03e884078"     # leaf voter (Base gauges)
FACTORY_REGISTRY = "0x7b2A2c12dC5Ea2a31455952a05031C3E4BB28F3E"
VOTING_ESCROW = "0xBF7977E1521e9a3032bf7243Ee4712f311f13915"
AERO_TOKEN = "0x3f5bF3EdAc59d650D8fDf9657a71F2f904A3a4Ee"      # new AERO

# ABIs (minimal)
REGISTRY_ABI = [
    {"type": "function", "name": "gaugeFactories", "stateMutability": "view", "inputs": [], "outputs": [{"type": "address[]"}]},
    {"type": "function", "name": "factoryToGauges", "stateMutability": "view", "inputs": [{"type": "address"}], "outputs": [{"type": "address[]"}]},
    {"type": "function", "name": "gaugeToTarget", "stateMutability": "view", "inputs": [{"type": "address"}], "outputs": [{"type": "address"}]},
    {"type": "function", "name": "gaugeToRewards", "stateMutability": "view", "inputs": [{"type": "address"}], "outputs": [{"type": "address"}]},
    {"type": "function", "name": "emissionCap", "stateMutability": "view", "inputs": [{"type": "address"}], "outputs": [{"type": "uint128"}]},
]

REWARDS_ABI = [
    {"type": "event", "name": "NotifyFeesAmount", "anonymous": False, "inputs": [
        {"name": "_gauge", "type": "address", "indexed": True},
        {"name": "_token", "type": "address", "indexed": True},
        {"name": "_amount", "type": "uint256", "indexed": False}]},
    {"type": "function", "name": "supplyAt", "stateMutability": "view", "inputs": [{"type": "uint256"}], "outputs": [{"type": "uint256"}]},
    {"type": "function", "name": "totalSupply", "stateMutability": "view", "inputs": [], "outputs": [{"type": "uint256"}]},
    {"type": "function", "name": "token0", "stateMutability": "view", "inputs": [], "outputs": [{"type": "address"}]},
    {"type": "function", "name": "token1", "stateMutability": "view", "inputs": [], "outputs": [{"type": "address"}]},
    {"type": "function", "name": "incentiveCount", "stateMutability": "view", "inputs": [], "outputs": [{"type": "uint256"}]},
    {"type": "function", "name": "incentive", "stateMutability": "view", "inputs": [{"type": "uint256"}], "outputs": [{"type": "tuple", "components": [
        {"name": "token", "type": "address"}, {"name": "amount", "type": "uint256"}, {"name": "rate", "type": "uint256"},
        {"name": "start", "type": "uint48"}, {"name": "end", "type": "uint48"}, {"name": "creator", "type": "address"}]}]},
    {"type": "function", "name": "feeRewardPerVotingPower", "stateMutability": "view", "inputs": [], "outputs": [
        {"type": "uint256"}, {"type": "uint256"}, {"type": "uint256"}, {"type": "uint256"}]},
]

LEAF_VOTER_ABI = [
    {"type": "function", "name": "allocationCooldown", "stateMutability": "view", "inputs": [], "outputs": [{"type": "uint48"}]},
    {"type": "function", "name": "gaugeStates", "stateMutability": "view", "inputs": [{"type": "address"}], "outputs": [
        {"name": "ceiling", "type": "uint128"}, {"name": "claimed", "type": "uint128"}, {"name": "lastSettlement", "type": "uint48"},
        {"name": "isRegistered", "type": "bool"}, {"name": "isActivated", "type": "bool"}, {"name": "surplus", "type": "uint128"},
        {"name": "lastIndex", "type": "uint256"}, {"name": "lastTimeIndex", "type": "uint256"},
        {"name": "point", "type": "tuple", "components": [
            {"name": "bias", "type": "int128"}, {"name": "slope", "type": "int128"}, {"name": "ts", "type": "uint48"},
            {"name": "permanentStakeBalance", "type": "uint128"}]}]},
]

ERC20_ABI = [
    {"type": "function", "name": "symbol", "stateMutability": "view", "inputs": [], "outputs": [{"type": "string"}]},
    {"type": "function", "name": "decimals", "stateMutability": "view", "inputs": [], "outputs": [{"type": "uint8"}]},
]

POOL_ABI = [
    {"type": "function", "name": "token0", "stateMutability": "view", "inputs": [], "outputs": [{"type": "address"}]},
    {"type": "function", "name": "token1", "stateMutability": "view", "inputs": [], "outputs": [{"type": "address"}]},
    {"type": "function", "name": "tickSpacing", "stateMutability": "view", "inputs": [], "outputs": [{"type": "int24"}]},
    {"type": "function", "name": "symbol", "stateMutability": "view", "inputs": [], "outputs": [{"type": "string"}]},
]

MULTICALL3_ABI = [
    {"type": "function", "name": "aggregate3", "stateMutability": "payable",
     "inputs": [{"name": "calls", "type": "tuple[]", "components": [
         {"name": "target", "type": "address"}, {"name": "allowFailure", "type": "bool"}, {"name": "callData", "type": "bytes"}]}],
     "outputs": [{"name": "returnData", "type": "tuple[]", "components": [
         {"name": "success", "type": "bool"}, {"name": "returnData", "type": "bytes"}]}]},
]
