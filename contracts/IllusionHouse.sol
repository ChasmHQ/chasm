// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;
pragma abicoder v1;
// 0x4b431d6d4014079f1883167dc6022231e2d4c328
// payload 0xf5b1e981000000000000000000000001B77719D643E7dE6b5fc0c59E74Ce0904386C391B00000000000000000000000000000000000000000000000000000000000000203078416e616e206f722054656e7361693f000000000000000000000000000000
// Blok Memori,Posisi Byte,Isi Data,Keterangan
// Selector,00 - 04,0x2e5e787e,ID Fungsi
// Arg 1,04 - 36,[Address Patron],Data Address Patron
// Arg 2,36 - 68,0x00...0040 (64),Offset (Penunjuk). Menunjuk ke byte 64 relatif (posisi 68 absolut).
// Length,68 - 100,0x00...0020 (32),Panjang Data Sigil. (Solidity butuh ini).
// Content,100 - 132,[Isi Password],Isi Data Sigil.
// https://anan.rocks/My-Challenges/0xL4ugh-CTF-v5/web3/House-of-Illusions
contract IllusionHouse {
    enum Role {
        Visitor,
        Curator
    }
    mapping(address => Role) public roles;
    mapping(address => uint96) public maskRank;
    mapping(address => bool) public admitted;
    bool public opened;
    bytes32 public constant SIGIL_PREIMAGE = bytes32("0xAnan or Tensai?");
    bytes32 public constant SIGIL_HASH =
        keccak256(abi.encodePacked(SIGIL_PREIMAGE));
    constructor() payable {}
    function initialize(address curator) external payable {
        require(!opened, "opened");
        opened = true;
        roles[address(this)] = Role.Curator;
        admitted[address(this)] = true;
    }
    function admit(address patron, bytes calldata sigil) external {
        require(!admitted[msg.sender], "already admitted");
        require(msg.data.length == 4 + 96, "invalid sigil payload");
        require(
            uint256(bytes32(msg.data[36:68])) == 0x20,
            "invalid sigil offset"
        );
        uint256 patronWord = uint256(bytes32(msg.data[4:36]));
        require(patronWord >> 160 != 0, "invalid patron encoding");
        require(roles[patron] == Role.Curator, "invalid patron");
        require(sigil.length == 32, "invalid sigil length");
        require(keccak256(sigil) == SIGIL_HASH, "invalid sigil");
        bytes32 sigilWord = abi.decode(sigil, (bytes32));
        uint96 rank = uint96(uint256(sigilWord) >> 160);
        admitted[msg.sender] = true;
        roles[msg.sender] = Role.Visitor;
        if (rank > 0) {
            maskRank[msg.sender] = rank;
        }
    }
    function appointCurator(address newCurator) external {
        require(maskRank[msg.sender] > 0, "not masked");
        roles[newCurator] = Role.Curator;
        admitted[newCurator] = true;
    }
}