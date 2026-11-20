// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

import {Test, console} from "forge-std/Test.sol";
import {Bank} from "../src/Bank.sol";

contract BankTest is Test {
    Bank bank;
    address alice = address(0xA11CE);

    function setUp() public {
        bank = new Bank("bank");
        vm.deal(alice, 10 ether);
    }

    function testDeposit() public {
        vm.prank(alice);
        bank.deposit{value: 2 ether}();
        assertEq(bank.balances(alice), 1.995 ether);
    }

    function testFeeIsTakenOnDeposit() public {
        vm.prank(alice);
        bank.deposit{value: 1 ether}();
        console.log("total deposits", bank.totalDeposits());
        assertEq(bank.totalDeposits(), 1.9975 ether);
    }
}
