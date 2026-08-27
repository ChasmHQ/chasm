// SPDX-License-Identifier: MIT
pragma solidity >=0.6.2 <0.9.0;

// Minimal forge-std `Vm` interface bundled with sevm.
//
// This is NOT the full forge-std interface. It declares the cheatcodes sevm's debugger
// implements (see src/sevm/cheatcodes.py), plus a few common signatures so that a
// standalone `Test.sol` still compiles. Any cheatcode declared here but not implemented by
// the engine reverts at runtime with "sevm: unimplemented cheatcode", which is a clearer
// signal than a compile error.
//
// The cheatcode address is the last 20 bytes of keccak256("hevm cheat code").
interface Vm {
    // ---- environment ----
    function warp(uint256 newTimestamp) external;
    function roll(uint256 newHeight) external;
    function fee(uint256 newBasefee) external;
    function chainId(uint256 newChainId) external;
    function coinbase(address newCoinbase) external;

    // ---- account state ----
    function deal(address account, uint256 newBalance) external;
    function etch(address target, bytes calldata newRuntimeBytecode) external;
    function store(address target, bytes32 slot, bytes32 value) external;
    function load(address target, bytes32 slot) external view returns (bytes32 data);

    // ---- identity / msg.sender ----
    function prank(address msgSender) external;
    function startPrank(address msgSender) external;
    function stopPrank() external;

    // ---- keys / signing ----
    function addr(uint256 privateKey) external pure returns (address keyAddr);
    function sign(uint256 privateKey, bytes32 digest)
        external
        pure
        returns (uint8 v, bytes32 r, bytes32 s);

    // ---- fuzzing / labelling ----
    function assume(bool condition) external pure;
    function label(address account, string calldata newLabel) external;

    // ---- declared for compile-compatibility, not implemented by the engine (v1) ----
    function prank(address msgSender, address txOrigin) external;
    function startPrank(address msgSender, address txOrigin) external;
    function expectRevert() external;
    function expectRevert(bytes4 revertData) external;
    function expectRevert(bytes calldata revertData) external;
    function expectEmit() external;
    function expectEmit(bool checkTopic1, bool checkTopic2, bool checkTopic3, bool checkData)
        external;
    function expectCall(address callee, bytes calldata data) external;
    function mockCall(address callee, bytes calldata data, bytes calldata returnData) external;
}
