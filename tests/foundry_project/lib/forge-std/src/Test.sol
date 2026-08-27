// SPDX-License-Identifier: MIT
pragma solidity >=0.6.2 <0.9.0;

import {Vm} from "./Vm.sol";
import {console} from "./console.sol";

// Minimal forge-std `Test` bundled with sevm. Enough to write and debug a standalone test:
// the `vm` cheatcode handle and revert-on-failure assertions.
//
// NOTE: unlike real forge-std, these assertions REVERT on failure rather than recording a
// `failed` flag and continuing. That is deliberate for a debugger: a failed assertion stops
// the transaction on the spot with a clear reason. Point sevm at a real Foundry project to
// get real forge-std semantics.
abstract contract Test {
    Vm internal constant vm = Vm(0x7109709ECfa91a80626fF3989D68f67F5b1DD12D);

    function assertTrue(bool condition) internal pure {
        require(condition, "assertTrue failed");
    }

    function assertTrue(bool condition, string memory err) internal pure {
        require(condition, err);
    }

    function assertFalse(bool condition) internal pure {
        require(!condition, "assertFalse failed");
    }

    function assertEq(uint256 a, uint256 b) internal pure {
        require(a == b, "assertEq(uint256) failed");
    }

    function assertEq(uint256 a, uint256 b, string memory err) internal pure {
        require(a == b, err);
    }

    function assertEq(int256 a, int256 b) internal pure {
        require(a == b, "assertEq(int256) failed");
    }

    function assertEq(address a, address b) internal pure {
        require(a == b, "assertEq(address) failed");
    }

    function assertEq(bool a, bool b) internal pure {
        require(a == b, "assertEq(bool) failed");
    }

    function assertEq(bytes32 a, bytes32 b) internal pure {
        require(a == b, "assertEq(bytes32) failed");
    }

    function assertEq(string memory a, string memory b) internal pure {
        require(
            keccak256(bytes(a)) == keccak256(bytes(b)), "assertEq(string) failed"
        );
    }

    function assertGt(uint256 a, uint256 b) internal pure {
        require(a > b, "assertGt failed");
    }

    function assertGe(uint256 a, uint256 b) internal pure {
        require(a >= b, "assertGe failed");
    }

    function assertLt(uint256 a, uint256 b) internal pure {
        require(a < b, "assertLt failed");
    }

    function assertLe(uint256 a, uint256 b) internal pure {
        require(a <= b, "assertLe failed");
    }
}
