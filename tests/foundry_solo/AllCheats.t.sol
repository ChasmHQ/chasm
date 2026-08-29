// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {Test} from "forge-std/Test.sol";
import {console} from "forge-std/console.sol";

contract Recorder {
    address public lastSender; // storage slot 0
    address public lastOrigin;
    function poke() external {
        lastSender = msg.sender;
        lastOrigin = tx.origin;
    }
}

contract Who {
    // Reports the frame it is called in; used to prove a delegate prank rewrites both.
    function ctx() external view returns (address sender, address self) {
        return (msg.sender, address(this));
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
    Who who;
    address alice = address(0xA11CE);
    address bob = address(0xB0B);

    function setUp() public {
        r = new Recorder();
        vault = new Vault();
        who = new Who();
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

    function testBlockGetters() public {
        vm.warp(1234);
        vm.roll(56);
        vm.chainId(7);
        assertEq(vm.getBlockTimestamp(), 1234);
        assertEq(vm.getBlockNumber(), 56);
        assertEq(vm.getChainId(), 7);
        vm.prevrandao(bytes32(uint256(0x99)));
        assertEq(block.prevrandao, 0x99);
    }

    function testFeeIsNotChargedAtSettlement() public {
        // A base fee above the transaction's own gas price would leave py-evm paying the
        // coinbase a negative amount at the end of the transaction, and a fresh coinbase
        // cannot go negative. The cheat must be visible here and gone by settlement.
        vm.fee(7 gwei);
        vm.coinbase(alice);
        assertEq(block.basefee, 7 gwei);
        assertEq(block.coinbase, alice);
    }

    function testNonceCheats() public {
        assertEq(vm.getNonce(alice), 0);
        vm.setNonce(alice, 5);
        assertEq(vm.getNonce(alice), 5);
        vm.setNonceUnsafe(alice, 2);
        assertEq(vm.getNonce(alice), 2);
        vm.resetNonce(alice);
        assertEq(vm.getNonce(alice), 0);
    }

    function testLabelLookup() public {
        vm.label(alice, "alice");
        assertEq(vm.getLabel(alice), "alice");
        assertEq(vm.getLabel(bob), "unlabeled:0x0000000000000000000000000000000000000B0b");
    }

    function testPrankOrigin() public {
        vm.prank(alice, bob);
        r.poke();
        assertEq(r.lastSender(), alice);
        assertEq(r.lastOrigin(), bob);
    }

    function testDelegatePrank() public {
        // A delegate prank rewrites msg.sender *and* address(this) inside the delegated
        // frame; a plain prank leaves a DELEGATECALL alone.
        (bool plain, bytes memory before) =
            address(who).delegatecall(abi.encodeWithSignature("ctx()"));
        assertTrue(plain);
        (, address selfBefore) = abi.decode(before, (address, address));
        assertEq(selfBefore, address(this));

        vm.prank(address(r), true);
        (bool ok, bytes memory out) =
            address(who).delegatecall(abi.encodeWithSignature("ctx()"));
        assertTrue(ok);
        (address sender, address self) = abi.decode(out, (address, address));
        assertEq(sender, address(r));
        assertEq(self, address(r));
    }

    function testStartPrankDelegate() public {
        vm.startPrank(address(r), true);
        for (uint256 i = 0; i < 2; i++) {
            (bool ok, bytes memory out) =
                address(who).delegatecall(abi.encodeWithSignature("ctx()"));
            assertTrue(ok);
            (address sender,) = abi.decode(out, (address, address));
            assertEq(sender, address(r));
        }
        vm.stopPrank();
    }
}
