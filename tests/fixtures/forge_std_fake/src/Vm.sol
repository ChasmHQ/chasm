// SPDX-License-Identifier: MIT
pragma solidity >=0.6.2 <0.9.0;

// Stand-in for forge-std's Vm, shaped like the real one: the cheats sevm implements plus
// the assertion overloads Test.sol below delegates to. Only used by the test suite, so it
// stays small; the real interface is fetched from foundry-rs/forge-std at run time.
interface Vm {
    function warp(uint256 newTimestamp) external;
    function roll(uint256 newHeight) external;
    function fee(uint256 newBasefee) external;
    function chainId(uint256 newChainId) external;
    function coinbase(address newCoinbase) external;
    function deal(address account, uint256 newBalance) external;
    function etch(address target, bytes calldata newRuntimeBytecode) external;
    function store(address target, bytes32 slot, bytes32 value) external;
    function load(address target, bytes32 slot) external view returns (bytes32 data);
    function prank(address msgSender) external;
    function prank(address msgSender, bool delegateCall) external;
    function prank(address msgSender, address txOrigin) external;
    function startPrank(address msgSender) external;
    function startPrank(address msgSender, bool delegateCall) external;
    function stopPrank() external;
    function getNonce(address account) external view returns (uint64 nonce);
    function setNonce(address account, uint64 newNonce) external;
    function setNonceUnsafe(address account, uint64 newNonce) external;
    function resetNonce(address account) external;
    function getBlockNumber() external view returns (uint256 height);
    function getBlockTimestamp() external view returns (uint256 timestamp);
    function getChainId() external view returns (uint256 chainId);
    function prevrandao(bytes32 newPrevrandao) external;
    function getLabel(address account) external view returns (string memory currentLabel);
    function addr(uint256 privateKey) external pure returns (address keyAddr);
    function sign(uint256 privateKey, bytes32 digest)
        external
        pure
        returns (uint8 v, bytes32 r, bytes32 s);
    function assume(bool condition) external pure;
    function label(address account, string calldata newLabel) external;

    function assertTrue(bool data) external pure;
    function assertFalse(bool data) external pure;
    function assertEq(uint256 left, uint256 right) external pure;
    function assertEq(uint256 left, uint256 right, string calldata err) external pure;
    function assertEq(int256 left, int256 right) external pure;
    function assertEq(address left, address right) external pure;
    function assertEq(bool left, bool right) external pure;
    function assertEq(bytes32 left, bytes32 right) external pure;
    function assertEq(string calldata left, string calldata right) external pure;
    function assertNotEq(uint256 left, uint256 right) external pure;
    function assertEqDecimal(uint256 left, uint256 right, uint256 decimals) external pure;
    function assertGt(uint256 left, uint256 right) external pure;
    function assertGe(uint256 left, uint256 right) external pure;
    function assertLt(uint256 left, uint256 right) external pure;
    function assertLe(uint256 left, uint256 right) external pure;
    function assertApproxEqAbs(uint256 left, uint256 right, uint256 maxDelta) external pure;
    function assertApproxEqRel(uint256 left, uint256 right, uint256 maxPercentDelta)
        external
        pure;
}
