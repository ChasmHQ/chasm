// SPDX-License-Identifier: MIT
pragma solidity ^0.8.13;

// A relative import, like OpenZeppelin Upgradeable: a second copy of the
// library brings its own Init.
import {Init} from "../base/Init.sol";

abstract contract Owned is Init {
    address public owner;

    function _initOwner(address who) internal {
        owner = who;
    }
}
