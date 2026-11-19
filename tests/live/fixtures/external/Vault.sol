// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.13;

contract Vault {
    address public immutable owner;
    uint256 public total;

    constructor(address _owner) {
        owner = _owner;
    }

    function deposit(uint256 amount) external {
        uint256 next = total + amount;
        total = next;
    }
}
