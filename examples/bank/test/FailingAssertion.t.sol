// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

import {Test} from "forge-std/Test.sol";
import {Bank} from "../src/Bank.sol";

contract FailingAssertionTest is Test {
    Bank bank;
    address alice = address(0xA11CE);

    function setUp() public {
        bank = new Bank("bank");
        vm.deal(alice, 10 ether);
    }

    /// Wrong on purpose: Bank takes a 25 bps fee, so alice is credited 0.9975 ether.
    function testBalanceIgnoresFee() public {
        vm.prank(alice);
        bank.deposit{value: 1 ether}();
        assertEq(bank.balances(alice), 1 ether);
    }
}
