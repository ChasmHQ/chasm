// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

/// @title Vault - a deliberately buggy contract for EVM-level debugging practice.
/// @notice The article uses Py-EVM to chase low-level EVM/memory bugs that Foundry's
///         read-only debugger struggles with. This contract seeds one such bug so the
///         opcode tracer has something concrete to reveal.
contract Vault {
    address public owner;   // storage slot 0
    uint256 public num;     // storage slot 1

    constructor() payable {
        owner = msg.sender;
        num = msg.value;
    }

    /// Plain setter - the "happy path" used to sanity-check the harness.
    function setNum(uint256 n) public {
        num = n;
    }

    /// @dev THE BUG: writes `values[idx]` to a raw storage slot computed from a
    ///      fixed base plus idx, with NO bounds relationship to the intended array.
    ///      For the right idx this collides with slot 0 (owner), letting anyone
    ///      overwrite `owner`. This is the classic "arbitrary storage write" pattern.
    ///      Reproduces cleanly at the EVM level: watch the SSTORE target slot.
    function unsafeStore(uint256 slot, uint256 value) public {
        assembly {
            sstore(slot, value)
        }
    }

    /// Convenience read of an arbitrary slot, for verifying the write landed.
    function readSlot(uint256 slot) public view returns (uint256 v) {
        assembly {
            v := sload(slot)
        }
    }
}
