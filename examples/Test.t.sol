// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.28;

import {Test, console} from "forge-std/Test.sol";


contract DebugTest is Test {
    Bank bank;
    address player = address(0xdead);

    function setUp() public {
        bank = new Bank("bank");
        vm.deal(player, 1 ether);
    }

    function testDeposit() public {
        vm.startPrank(player);
        bank.deposit{ value: 1 ether }();
        vm.stopPrank();
    }
}


contract Bank {
    struct Account {
        uint128 balance;
        bool frozen;
        string nickname;
    }

    address public owner;
    uint96 public feeBps;
    uint256 public totalDeposits;
    mapping(address => uint256) public balances;
    mapping(address => Account) public accounts;
    uint256[] public history;
    string public name;

    event Deposited(address indexed who, uint256 amount);

    error NotOwner(address caller);

    modifier onlyOwner() {
        if (msg.sender != owner) revert NotOwner(msg.sender);
        _;
    }

    constructor(string memory _name) payable {
        owner = msg.sender;
        feeBps = 25;
        name = _name;
        balances[msg.sender] = msg.value;
        totalDeposits = msg.value;
    }

    function _fee(uint256 amount) internal view returns (uint256) {
        return (amount * feeBps) / 10000;
    }

    function _credit(address who, uint256 amount) internal {
        uint256 fee = _fee(amount);
        balances[who] += amount - fee;
        totalDeposits += amount - fee;
        history.push(amount);
    }

    function deposit() public payable {
        _credit(msg.sender, msg.value);
        emit Deposited(msg.sender, msg.value);
    }

    function setNickname(string memory nick) public {
        accounts[msg.sender].nickname = nick;
        accounts[msg.sender].balance = uint128(balances[msg.sender]);
    }

    function freeze(address who) public onlyOwner {
        accounts[who].frozen = true;
    }

    function sumHistory() public view returns (uint256 total) {
        for (uint256 i = 0; i < history.length; i++) {
            total += history[i];
        }
    }

    function withdraw(uint256 amount) public {
        require(balances[msg.sender] >= amount, "insufficient balance");
        balances[msg.sender] -= amount;
        totalDeposits -= amount;
    }

    function forward(address target, uint256 value) public returns (uint256) {
        return Callee(target).receiveValue(value);
    }

    function boom() public pure {
        revert("kaboom");
    }

    function overflow(uint256 a, uint256 b) public pure returns (uint256) {
        return a - b;
    }
}

contract Callee {
    uint256 public last;

    function receiveValue(uint256 v) public returns (uint256) {
        last = v;
        return v * 2;
    }
}
