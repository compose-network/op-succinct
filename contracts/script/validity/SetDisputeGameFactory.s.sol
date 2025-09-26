// SPDX-License-Identifier: MIT
pragma solidity ^0.8.15;

import {Script} from "forge-std/Script.sol";
import {console} from "forge-std/console.sol";

// Interface for the contract that has setDisputeGameFactory function
interface IDisputeGameFactorySetter {
    function setDisputeGameFactory(address _disputeGameFactory) external;
    function disputeGameFactory() external view returns (address);
    function owner() external view returns (address);
}

contract SetDisputeGameFactory is Script {
    function run() public {
        // Load environment variables
        address targetContractAddress = vm.envAddress("L2OO_ADDRESS");
        address disputeGameFactoryAddress = vm.envAddress("DGF_ADDRESS");

        console.log("Target Contract Address:", targetContractAddress);
        console.log("DisputeGameFactory Address:", disputeGameFactoryAddress);

        vm.startBroadcast();

        // Get the target contract
        IDisputeGameFactorySetter targetContract = IDisputeGameFactorySetter(targetContractAddress);

        // Check current dispute game factory (optional)
        try targetContract.disputeGameFactory() returns (address currentDGF) {
            console.log("Current DisputeGameFactory:", currentDGF);
            if (currentDGF == disputeGameFactoryAddress) {
                console.log("Warning: DisputeGameFactory is already set to this address");
            }
        } catch {
            console.log("Could not read current disputeGameFactory (may not have getter)");
        }

        // Check if caller is owner (optional verification)
        try targetContract.owner() returns (address owner) {
            console.log("Contract owner:", owner);
            console.log("Caller (msg.sender):", msg.sender);
            if (owner != msg.sender) {
                console.log("Warning: Caller is not the owner. Transaction may fail.");
            }
        } catch {
            console.log("Could not read owner (may not have owner function)");
        }

        // Set the dispute game factory
        targetContract.setDisputeGameFactory(disputeGameFactoryAddress);

        console.log("Successfully called setDisputeGameFactory");

        // Verify the setting was successful (optional)
        try targetContract.disputeGameFactory() returns (address newDGF) {
            require(newDGF == disputeGameFactoryAddress, "DisputeGameFactory not set correctly");
            console.log("Verification passed. DisputeGameFactory set to:", newDGF);
        } catch {
            console.log("Could not verify setting (may not have getter function)");
        }

        vm.stopBroadcast();
    }

    // Helper function to get the dispute game factory address from env
    function getDisputeGameFactoryAddress() public view returns (address) {
        return vm.envAddress("DGF_ADDRESS");
    }

    // Helper function to get the target contract address from env
    function getTargetContractAddress() public view returns (address) {
        return vm.envAddress("L2OO_ADDRESS");
    }
}