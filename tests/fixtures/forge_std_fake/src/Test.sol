// SPDX-License-Identifier: MIT
pragma solidity >=0.6.2 <0.9.0;

import {Vm} from "./Vm.sol";
import {console} from "./console.sol";

// Mirrors real forge-std: every assertion is a guard plus a `vm.assert*` cheatcode call,
// so the suite exercises sevm's assertion engine rather than plain Solidity requires.
abstract contract Test {
    Vm internal constant vm = Vm(0x7109709ECfa91a80626fF3989D68f67F5b1DD12D);

    function assertTrue(bool condition) internal pure {
        if (!condition) {
            vm.assertTrue(condition);
        }
    }

    function assertFalse(bool condition) internal pure {
        if (condition) {
            vm.assertFalse(condition);
        }
    }

    function assertEq(uint256 a, uint256 b) internal pure {
        if (a != b) {
            vm.assertEq(a, b);
        }
    }

    function assertEq(uint256 a, uint256 b, string memory err) internal pure {
        if (a != b) {
            vm.assertEq(a, b, err);
        }
    }

    function assertEq(int256 a, int256 b) internal pure {
        if (a != b) {
            vm.assertEq(a, b);
        }
    }

    function assertEq(address a, address b) internal pure {
        if (a != b) {
            vm.assertEq(a, b);
        }
    }

    function assertEq(bool a, bool b) internal pure {
        if (a != b) {
            vm.assertEq(a, b);
        }
    }

    function assertEq(bytes32 a, bytes32 b) internal pure {
        if (a != b) {
            vm.assertEq(a, b);
        }
    }

    function assertEq(string memory a, string memory b) internal pure {
        if (keccak256(bytes(a)) != keccak256(bytes(b))) {
            vm.assertEq(a, b);
        }
    }

    function assertNotEq(uint256 a, uint256 b) internal pure {
        if (a == b) {
            vm.assertNotEq(a, b);
        }
    }

    function assertEqDecimal(uint256 a, uint256 b, uint256 decimals) internal pure {
        vm.assertEqDecimal(a, b, decimals);
    }

    function assertGt(uint256 a, uint256 b) internal pure {
        if (a <= b) {
            vm.assertGt(a, b);
        }
    }

    function assertGe(uint256 a, uint256 b) internal pure {
        if (a < b) {
            vm.assertGe(a, b);
        }
    }

    function assertLt(uint256 a, uint256 b) internal pure {
        if (a >= b) {
            vm.assertLt(a, b);
        }
    }

    function assertLe(uint256 a, uint256 b) internal pure {
        if (a > b) {
            vm.assertLe(a, b);
        }
    }

    function assertApproxEqAbs(uint256 a, uint256 b, uint256 maxDelta) internal pure {
        vm.assertApproxEqAbs(a, b, maxDelta);
    }

    function assertApproxEqRel(uint256 a, uint256 b, uint256 maxPercentDelta)
        internal
        pure
    {
        vm.assertApproxEqRel(a, b, maxPercentDelta);
    }
}
