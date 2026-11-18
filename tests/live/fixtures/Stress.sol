// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.13;

/// Twenty arguments, so the request form has twenty fields. An interface:
/// a function this wide does not compile (stack too deep), but the form
/// only needs the ABI.
interface IWide {
    function wide(
        uint8 a01, uint16 a02, uint32 a03, uint64 a04, uint128 a05,
        uint256 a06, int8 a07, int256 a08, bool a09, address a10,
        bytes32 a11, bytes4 a12, uint256 a13, uint256 a14, uint256 a15,
        uint256 a16, uint256 a17, uint256 a18, uint256 a19, uint256 a20
    ) external;
}

/// Fixture for tests/live `stress`: long arrays and long traces.
contract Stress {
    mapping(uint256 => uint256) public slots;
    uint256 public total;

    /// A 50-item array argument.
    function sum(uint256[] calldata values) external returns (uint256 result) {
        for (uint256 i = 0; i < values.length; i++) {
            result += values[i];
        }
        total = result;
    }

    /// `n` storage writes; the step count grows with n.
    function churn(uint256 n) external {
        for (uint256 i = 0; i < n; i++) {
            slots[i] = i + 1;
        }
    }

    /// `n` loop iterations without storage, for long traces with small state.
    function spin(uint256 n) external returns (uint256 acc) {
        for (uint256 i = 0; i < n; i++) {
            unchecked {
                acc = acc * 31 + i;
            }
        }
        total = acc;
    }
}
