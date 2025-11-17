// SPDX-License-Identifier: MIT
pragma solidity ^0.8.15;

import {Script} from "forge-std/Script.sol";
import {console} from "forge-std/console.sol";
import {OPSuccinctDisputeGame} from "../../src/validity/OPSuccinctDisputeGame.sol";
import {DisputeGameFactory} from "src/dispute/DisputeGameFactory.sol";
import {GameType, GameTypes} from "src/dispute/lib/Types.sol";
import {IDisputeGame} from "interfaces/dispute/IDisputeGame.sol";

contract SetOPSuccinctDisputeGameImpl is Script {
    function run() public {
        // Load environment variables
        address factoryAddress = vm.envAddress("DGF_ADDRESS");
        address l2OutputOracleAddress = vm.envAddress("L2OO_ADDRESS");
        
        // Get the game type for OP_SUCCINCT (typically 6)
        GameType gameType = GameTypes.OP_SUCCINCT;
        
        console.log("Factory Address:", factoryAddress);
        console.log("L2OutputOracle Address:", l2OutputOracleAddress);
        console.log("Game Type:", GameType.unwrap(gameType));
        
        vm.startBroadcast();
        
        // Deploy OPSuccinctDisputeGame implementation
        OPSuccinctDisputeGame gameImpl = new OPSuccinctDisputeGame(l2OutputOracleAddress);
        console.log("Deployed OPSuccinctDisputeGame at:", address(gameImpl));
        
        // Get the factory contract
        DisputeGameFactory factory = DisputeGameFactory(factoryAddress);
        
        // Check if implementation is already set
        address currentImpl = address(factory.gameImpls(gameType));
        if (currentImpl != address(0)) {
            console.log("Warning: Game type", GameType.unwrap(gameType), "already has implementation:", currentImpl);
            console.log("This will overwrite the existing implementation.");
        }
        
        // Set the new implementation
        factory.setImplementation(gameType, IDisputeGame(address(gameImpl)));
        
        console.log("Successfully set OPSuccinctDisputeGame implementation for game type:", GameType.unwrap(gameType));
        
        // Verify the implementation was set correctly
        address newImpl = address(factory.gameImpls(gameType));
        require(newImpl == address(gameImpl), "Implementation not set correctly");
        console.log("Verification passed. Implementation set to:", newImpl);
        
        vm.stopBroadcast();
    }
    
    function getGameType() public pure returns (uint32) {
        return GameType.unwrap(GameTypes.OP_SUCCINCT);
    }
} 