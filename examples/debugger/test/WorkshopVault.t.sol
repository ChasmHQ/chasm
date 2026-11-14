// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {WorkshopVault} from "../src/WorkshopVault.sol";

interface Vm {
    function deal(address account, uint256 balance) external;
}

contract WorkshopVaultTest {
    Vm constant vm = Vm(address(uint160(uint256(keccak256("hevm cheat code")))));
    WorkshopVault vault;

    function setUp() public {
        vm.deal(address(this), 1 ether);
        vault = new WorkshopVault();
        vault.deposit{value: 10000}();
    }

    function testWithdraw() public {
        vault.withdraw(1000);
        require(vault.balances(address(this)) == 9000, "unexpected balance");
        require(vault.total() == 9002, "unexpected total");
    }
}
