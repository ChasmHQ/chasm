// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.13;

contract Bank {
    mapping(address => uint256) public balanceOf;
    address private owner;
    uint256[] private luckyNumbers;

    constructor(address _owner, uint256[] memory _luckyNumbers) {
        owner = _owner;
        luckyNumbers = _luckyNumbers;
    }

    modifier onlyOwner() {
        require(msg.sender == owner, "Only owner can call this function");
        _;
    }

    function deposit() public payable {
        balanceOf[msg.sender] += msg.value;
    }

    function withdraw(uint256 _amount) public {
        require(
            _amount <= balanceOf[msg.sender],
            "Amount to Withdraw exceed Balance"
        );
        require(_amount >= 1 ether, "Minimum Withdrawal is 1 Ether");
        balanceOf[msg.sender] = balanceOf[msg.sender] - _amount;
        (bool sent, ) = msg.sender.call{value: _amount}("");
        require(sent, "Withdrawal Failed!");
    }

    function transferOwnership(address _newOwner) public onlyOwner {
        owner = _newOwner;
    }
}