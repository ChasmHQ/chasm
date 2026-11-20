// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {Test} from "forge-std/Test.sol";

import {Token} from "../src/Token.sol";

contract TokenTest is Test {
    Token token;
    address bob = address(0xB0B);

    function setUp() public {
        token = new Token();
    }

    function testMintAsOwner() public {
        // this test contract deployed the token, so it is owner
        token.mint(bob, 100);
        assertEq(token.balanceOf(bob), 100);
    }

    function testMintPrankRevertsForNonOwner() public {
        // prank as bob (not owner): mint must revert
        vm.prank(bob);
        try token.mint(bob, 1) {
            revert("should have reverted");
        } catch {}
        assertEq(token.balanceOf(bob), 0);
    }
}
