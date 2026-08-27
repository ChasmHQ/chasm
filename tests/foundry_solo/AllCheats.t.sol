// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {Test} from "forge-std/Test.sol";
import {console} from "forge-std/console.sol";

contract Recorder {
    address public lastSender; // storage slot 0
    function poke() external {
        lastSender = msg.sender;
    }
}

contract Vault {
    mapping(address => uint256) public deposits;
    function deposit() external payable {
        deposits[msg.sender] += msg.value;
    }
}

contract AllCheatsTest is Test {
    Recorder r;
    Vault vault;
    address alice = address(0xA11CE);

    function setUp() public {
        r = new Recorder();
        vault = new Vault();
    }

    function testPrankValue() public {
        // A value-bearing call under a prank must draw its value from the pranked
        // address, exactly like forge, not from this test contract.
        vm.deal(alice, 3 ether);
        vm.prank(alice);
        vault.deposit{value: 2 ether}();
        assertEq(vault.deposits(alice), 2 ether); // credited to msg.sender = alice
        assertEq(alice.balance, 1 ether); // value came from alice
        assertEq(address(vault).balance, 2 ether);
    }

    function testEnv() public {
        vm.deal(alice, 7 ether);
        assertEq(alice.balance, 7 ether);
        vm.warp(4242);
        assertEq(block.timestamp, 4242);
        vm.roll(99);
        assertEq(block.number, 99);
        vm.chainId(31337);
        assertEq(block.chainid, 31337);
        console.log("env ok at", block.timestamp);
    }

    function testPrank() public {
        vm.prank(alice);
        r.poke();
        assertEq(r.lastSender(), alice);
        // the call after a single prank is back to this contract
        r.poke();
        assertEq(r.lastSender(), address(this));
        // startPrank persists until stopPrank
        vm.startPrank(alice);
        r.poke();
        assertEq(r.lastSender(), alice);
        r.poke();
        assertEq(r.lastSender(), alice);
        vm.stopPrank();
        r.poke();
        assertEq(r.lastSender(), address(this));
    }

    function testStorageAndKeys() public {
        vm.store(address(r), bytes32(uint256(0)), bytes32(uint256(uint160(alice))));
        assertEq(r.lastSender(), alice);
        bytes32 got = vm.load(address(r), bytes32(uint256(0)));
        assertEq(uint256(got), uint256(uint160(alice)));
        address a1 = vm.addr(1);
        assertTrue(a1 != address(0));
        (uint8 v, , ) = vm.sign(1, keccak256("hi"));
        assertTrue(v == 27 || v == 28);
        vm.label(alice, "alice");
    }
}
