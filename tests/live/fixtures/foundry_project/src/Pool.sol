// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.13;

import {Init} from "@shared/base/Init.sol";
import {Owned} from "@shared/upgradeable/Owned.sol";

/// Fixture for tests/live `build_source`: both imports must resolve to one Init.
contract Pool is Init, Owned {
    uint256 public total;

    function initialize(address who) external initializer {
        _initOwner(who);
    }

    function add(uint256 amount) external {
        total += amount;
    }
}
