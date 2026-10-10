//! OpenAPI / JSON Schema generation for KVNC JSON-RPC.
//!
//! Generates an OpenAPI 3.1 specification from the RPC method definitions.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// OpenAPI 3.1 specification structure.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct OpenApiSpec {
    pub openapi: String,
    pub info: OpenApiInfo,
    pub servers: Vec<OpenApiServer>,
    pub paths: BTreeMap<String, OpenApiPathItem>,
    pub components: OpenApiComponents,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct OpenApiInfo {
    pub title: String,
    pub version: String,
    pub description: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct OpenApiServer {
    pub url: String,
    pub description: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct OpenApiPathItem {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub post: Option<OpenApiOperation>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct OpenApiOperation {
    pub summary: String,
    pub description: String,
    #[serde(rename = "operationId")]
    pub operation_id: String,
    pub tags: Vec<String>,
    #[serde(rename = "requestBody")]
    pub request_body: Option<OpenApiRequestBody>,
    pub responses: BTreeMap<String, OpenApiResponse>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct OpenApiRequestBody {
    pub required: bool,
    pub content: BTreeMap<String, OpenApiMediaType>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct OpenApiMediaType {
    pub schema: Value,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct OpenApiResponse {
    pub description: String,
    pub content: BTreeMap<String, OpenApiMediaType>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct OpenApiComponents {
    pub schemas: BTreeMap<String, Value>,
}

/// Generate the OpenAPI 3.1 specification for KVNC JSON-RPC.
pub fn generate_openapi_spec() -> OpenApiSpec {
    let mut spec = OpenApiSpec {
        openapi: "3.1.0".to_string(),
        info: OpenApiInfo {
            title: "KVNC JSON-RPC API".to_string(),
            version: "0.1.0".to_string(),
            description: "JSON-RPC API for KVNC blockchain node. All methods use JSON-RPC 2.0 over HTTP POST to /rpc endpoint.".to_string(),
        },
        servers: vec![
            OpenApiServer {
                url: "http://localhost:8545/rpc".to_string(),
                description: "Local development node".to_string(),
            },
            OpenApiServer {
                url: "https://api.kovanica.online/rpc".to_string(),
                description: "Public mainnet node".to_string(),
            },
        ],
        paths: BTreeMap::new(),
        components: OpenApiComponents {
            schemas: BTreeMap::new(),
        },
    };

    // Add common schemas
    spec.components.schemas.insert(
        "JsonRpcRequest".to_string(),
        json!({
            "type": "object",
            "required": ["jsonrpc", "method"],
            "properties": {
                "jsonrpc": { "type": "string", "const": "2.0" },
                "method": { "type": "string" },
                "params": { "type": "array", "items": {} },
                "id": { "type": ["string", "number", "null"] }
            }
        }),
    );

    spec.components.schemas.insert(
        "JsonRpcResponse".to_string(),
        json!({
            "type": "object",
            "properties": {
                "jsonrpc": { "type": "string", "const": "2.0" },
                "result": {},
                "error": { "$ref": "#/components/schemas/JsonRpcError" },
                "id": { "type": ["string", "number", "null"] }
            }
        }),
    );

    spec.components.schemas.insert(
        "JsonRpcError".to_string(),
        json!({
            "type": "object",
            "required": ["code", "message"],
            "properties": {
                "code": { "type": "integer" },
                "message": { "type": "string" },
                "data": {}
            }
        }),
    );

    spec.components.schemas.insert(
        "Address".to_string(),
        json!({
            "type": "string",
            "pattern": "^kvnc[a-f0-9]{64}dag$",
            "description": "Canonical KVNC address format (kvnc<hex>dag with blake3 checksum)"
        }),
    );

    spec.components.schemas.insert(
        "Hash".to_string(),
        json!({
            "type": "string",
            "pattern": "^0x[a-f0-9]{64}$",
            "description": "32-byte hash as 0x-prefixed hex"
        }),
    );

    spec.components.schemas.insert(
        "Quantity".to_string(),
        json!({
            "type": "string",
            "pattern": "^0x[a-f0-9]+$",
            "description": "Ethereum-style quantity (minimal hex)"
        }),
    );

    spec.components.schemas.insert(
        "Transaction".to_string(),
        json!({
            "type": "object",
            "properties": {
                "hash": { "$ref": "#/components/schemas/Hash" },
                "sender": { "$ref": "#/components/schemas/Address" },
                "nonce": { "$ref": "#/components/schemas/Quantity" },
                "fee": { "$ref": "#/components/schemas/Quantity" },
                "kind": { "$ref": "#/components/schemas/TransactionKind" },
                "signature": { "type": "string", "pattern": "^0x[a-f0-9]{128}$" }
            }
        }),
    );

    spec.components.schemas.insert("TransactionKind".to_string(), json!({
        "oneOf": [
            { "type": "object", "properties": { "type": { "const": "transfer" }, "to": { "$ref": "#/components/schemas/Address" }, "amount": { "$ref": "#/components/schemas/Quantity" } } },
            { "type": "object", "properties": { "type": { "const": "stake" }, "amount": { "$ref": "#/components/schemas/Quantity" } } },
            { "type": "object", "properties": { "type": { "const": "unstake" }, "amount": { "$ref": "#/components/schemas/Quantity" } } },
            { "type": "object", "properties": { "type": { "const": "delegate" }, "validator": { "$ref": "#/components/schemas/Address" }, "amount": { "$ref": "#/components/schemas/Quantity" } } },
            { "type": "object", "properties": { "type": { "const": "claim_rewards" }, "validator": { "type": ["string", "null"] } } },
            { "type": "object", "properties": { "type": { "const": "deploy" }, "code": { "type": "string", "pattern": "^0x[a-f0-9]+$" } } },
            { "type": "object", "properties": { "type": { "const": "call" }, "contract": { "$ref": "#/components/schemas/Address" }, "method": { "type": "string" }, "args": { "type": "string", "pattern": "^0x[a-f0-9]*$" }, "gas_limit": { "$ref": "#/components/schemas/Quantity" } } }
        ]
    }));

    spec.components.schemas.insert("Block".to_string(), json!({
        "type": "object",
        "properties": {
            "hash": { "$ref": "#/components/schemas/Hash" },
            "author": { "$ref": "#/components/schemas/Address" },
            "round": { "$ref": "#/components/schemas/Quantity" },
            "parents": { "type": "array", "items": { "$ref": "#/components/schemas/Hash" } },
            "transactions": { "type": "array", "items": { "$ref": "#/components/schemas/Hash" } },
            "timestamp": { "$ref": "#/components/schemas/Quantity" }
        }
    }));

    spec.components.schemas.insert(
        "Account".to_string(),
        json!({
            "type": "object",
            "properties": {
                "address": { "$ref": "#/components/schemas/Address" },
                "balance": { "$ref": "#/components/schemas/Quantity" },
                "nonce": { "$ref": "#/components/schemas/Quantity" },
                "code_hash": { "$ref": "#/components/schemas/Hash" },
                "is_contract": { "type": "boolean" }
            }
        }),
    );

    spec.components.schemas.insert(
        "Validator".to_string(),
        json!({
            "type": "object",
            "properties": {
                "address": { "$ref": "#/components/schemas/Address" },
                "public_key": { "type": "string", "pattern": "^0x[a-f0-9]{64}$" },
                "stake": { "$ref": "#/components/schemas/Quantity" },
                "active": { "type": "boolean" },
                "commission_bps": { "type": "integer" }
            }
        }),
    );

    // Define all RPC methods
    let methods = vec![
        (
            "kvnc_getBlockByHash",
            "Get block by hash",
            "Chain",
            json!({
                "type": "array",
                "items": [
                    { "$ref": "#/components/schemas/Hash" },
                    { "type": "boolean" }
                ],
                "minItems": 1,
                "maxItems": 2
            }),
            json!({
                "type": "object",
                "properties": {
                    "block": { "$ref": "#/components/schemas/Block" },
                    "full_transactions": { "type": "boolean" }
                }
            }),
        ),
        (
            "kvnc_getBlockByNumber",
            "Get block by height/round",
            "Chain",
            json!({
                "type": "array",
                "items": [
                    { "$ref": "#/components/schemas/Quantity" },
                    { "type": "boolean" }
                ],
                "minItems": 1,
                "maxItems": 2
            }),
            json!({
                "type": "object",
                "properties": {
                    "block": { "$ref": "#/components/schemas/Block" },
                    "full_transactions": { "type": "boolean" }
                }
            }),
        ),
        (
            "kvnc_sendRawTransaction",
            "Submit signed transaction",
            "Transactions",
            json!({
                "type": "array",
                "items": { "type": "string", "pattern": "^0x[a-f0-9]+$" },
                "minItems": 1,
                "maxItems": 1
            }),
            json!({
                "type": "string",
                "pattern": "^0x[a-f0-9]{64}$",
                "description": "Transaction hash"
            }),
        ),
        (
            "kvnc_getTransactionReceipt",
            "Get transaction receipt",
            "Transactions",
            json!({
                "type": "array",
                "items": { "$ref": "#/components/schemas/Hash" },
                "minItems": 1,
                "maxItems": 1
            }),
            json!({
                "type": "object",
                "properties": {
                    "tx_hash": { "$ref": "#/components/schemas/Hash" },
                    "success": { "type": "boolean" },
                    "gas_used": { "$ref": "#/components/schemas/Quantity" },
                    "error": { "type": ["string", "null"] },
                    "events": { "type": "array", "items": { "type": "object" } }
                }
            }),
        ),
        (
            "kvnc_getTransactionByHash",
            "Get transaction by hash",
            "Transactions",
            json!({
                "type": "array",
                "items": { "$ref": "#/components/schemas/Hash" },
                "minItems": 1,
                "maxItems": 1
            }),
            json!({
                "$ref": "#/components/schemas/Transaction"
            }),
        ),
        (
            "kvnc_getBalance",
            "Get account balance",
            "Accounts",
            json!({
                "type": "array",
                "items": { "$ref": "#/components/schemas/Address" },
                "minItems": 1,
                "maxItems": 1
            }),
            json!({
                "$ref": "#/components/schemas/Quantity"
            }),
        ),
        (
            "kvnc_getNonce",
            "Get account nonce",
            "Accounts",
            json!({
                "type": "array",
                "items": { "$ref": "#/components/schemas/Address" },
                "minItems": 1,
                "maxItems": 1
            }),
            json!({
                "$ref": "#/components/schemas/Quantity"
            }),
        ),
        (
            "kvnc_getCode",
            "Get contract code",
            "Contracts",
            json!({
                "type": "array",
                "items": { "$ref": "#/components/schemas/Address" },
                "minItems": 1,
                "maxItems": 1
            }),
            json!({
                "type": "string",
                "pattern": "^0x[a-f0-9]*$"
            }),
        ),
        (
            "kvnc_getStorageAt",
            "Get contract storage slot",
            "Contracts",
            json!({
                "type": "array",
                "items": [
                    { "$ref": "#/components/schemas/Address" },
                    { "$ref": "#/components/schemas/Hash" }
                ],
                "minItems": 2,
                "maxItems": 2
            }),
            json!({
                "type": "string",
                "pattern": "^0x[a-f0-9]{64}$"
            }),
        ),
        (
            "kvnc_getValidators",
            "Get active validators",
            "Staking",
            json!({
                "type": "array",
                "items": {}
            }),
            json!({
                "type": "array",
                "items": { "$ref": "#/components/schemas/Validator" }
            }),
        ),
        (
            "kvnc_getStake",
            "Get total stake for address",
            "Staking",
            json!({
                "type": "array",
                "items": { "$ref": "#/components/schemas/Address" },
                "minItems": 1,
                "maxItems": 1
            }),
            json!({
                "$ref": "#/components/schemas/Quantity"
            }),
        ),
        (
            "kvnc_getRewards",
            "Get cumulative rewards",
            "Staking",
            json!({
                "type": "array",
                "items": { "$ref": "#/components/schemas/Address" },
                "minItems": 1,
                "maxItems": 1
            }),
            json!({
                "$ref": "#/components/schemas/Quantity"
            }),
        ),
        (
            "kvnc_getPendingTransactions",
            "Get pending transaction hashes",
            "Mempool",
            json!({
                "type": "array",
                "items": {}
            }),
            json!({
                "type": "array",
                "items": { "$ref": "#/components/schemas/Hash" }
            }),
        ),
        (
            "kvnc_estimateFee",
            "Estimate fee rate",
            "Mempool",
            json!({
                "type": "array",
                "items": {}
            }),
            json!({
                "$ref": "#/components/schemas/Quantity"
            }),
        ),
        (
            "kvnc_getLeaderSchedule",
            "Get upcoming leader schedule",
            "Consensus",
            json!({
                "type": "array",
                "items": { "type": "integer" },
                "minItems": 0,
                "maxItems": 1
            }),
            json!({
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "round": { "$ref": "#/components/schemas/Quantity" },
                        "leader": { "$ref": "#/components/schemas/Address" }
                    }
                }
            }),
        ),
        (
            "kvnc_getCommittee",
            "Get current committee",
            "Consensus",
            json!({
                "type": "array",
                "items": {}
            }),
            json!({
                "type": "object",
                "properties": {
                    "epoch": { "$ref": "#/components/schemas/Quantity" },
                    "authorities": {
                        "type": "array",
                        "items": { "$ref": "#/components/schemas/Validator" }
                    }
                }
            }),
        ),
        (
            "kvnc_blockNumber",
            "Get committed leader height",
            "Chain",
            json!({
                "type": "array",
                "items": {}
            }),
            json!({
                "$ref": "#/components/schemas/Quantity"
            }),
        ),
        // WebSocket subscription methods
        (
            "kvnc_subscribe",
            "Subscribe to events",
            "PubSub",
            json!({
                "type": "array",
                "items": { "type": "string", "enum": ["newHeads", "newCommittedLeader", "pendingTransactions", "logs"] },
                "minItems": 1,
                "maxItems": 1
            }),
            json!({
                "type": "string",
                "description": "Subscription ID"
            }),
        ),
        (
            "kvnc_unsubscribe",
            "Unsubscribe from events",
            "PubSub",
            json!({
                "type": "array",
                "items": { "type": "string" },
                "minItems": 1,
                "maxItems": 1
            }),
            json!({
                "type": "boolean"
            }),
        ),
    ];

    // Create method enum schema for oneOf discriminator
    let mut method_variants = Vec::new();
    for (method, summary, tag, params_schema, _result_schema) in &methods {
        method_variants.push(json!({
            "type": "object",
            "required": ["jsonrpc", "method", "params", "id"],
            "properties": {
                "jsonrpc": { "type": "string", "const": "2.0" },
                "method": { "type": "string", "const": method },
                "params": params_schema,
                "id": { "type": ["string", "number", "null"] }
            },
            "description": format!("{} - {}", method, summary),
            "x-tag": tag
        }));
    }

    // Create request body schema with oneOf for all methods
    let request_body_schema = json!({
        "oneOf": method_variants,
        "discriminator": {
            "propertyName": "method",
            "mapping": method_variants.iter().map(|v| {
                let method = v["properties"]["method"]["const"].as_str().unwrap();
                (method.to_string(), format!("#/components/schemas/JsonRpcRequest{}", method.replace("kvnc_", "")))
            }).collect::<std::collections::BTreeMap<_, _>>()
        }
    });

    // Create response schema with oneOf for all result types
    let mut response_variants = Vec::new();
    for (method, _, _, _, result_schema) in &methods {
        let result_schema_name = format!("{}Result", method.replace("kvnc_", "").replace(".", ""));
        // Add result schema to components
        spec.components
            .schemas
            .insert(result_schema_name.clone(), result_schema.clone());
        response_variants.push(json!({
            "type": "object",
            "required": ["jsonrpc", "result", "id"],
            "properties": {
                "jsonrpc": { "type": "string", "const": "2.0" },
                "result": { "$ref": format!("#/components/schemas/{}", result_schema_name) },
                "error": { "$ref": "#/components/schemas/JsonRpcError" },
                "id": { "type": ["string", "number", "null"] }
            }
        }));
    }

    let response_body_schema = json!({
        "oneOf": response_variants,
        "discriminator": {
            "propertyName": "result",
            "mapping": methods.iter().map(|(m, _, _, _, _)| {
                let name = m.replace("kvnc_", "").replace(".", "");
                (name.clone(), format!("#/components/schemas/{}Result", name))
            }).collect::<std::collections::BTreeMap<_, _>>()
        }
    });

    // Add the single /rpc endpoint
    let path = "/rpc";
    let operation = OpenApiOperation {
        summary: "KVNC JSON-RPC endpoint".to_string(),
        description: "All KVNC JSON-RPC 2.0 methods are available via this single endpoint. The method is specified in the request body's 'method' field.".to_string(),
        operation_id: "jsonrpc".to_string(),
        tags: vec!["JSON-RPC".to_string()],
        request_body: Some(OpenApiRequestBody {
            required: true,
            content: {
                let mut content = BTreeMap::new();
                content.insert("application/json".to_string(), OpenApiMediaType {
                    schema: request_body_schema
                });
                content
            },
        }),
        responses: {
            let mut responses = BTreeMap::new();
            responses.insert("200".to_string(), OpenApiResponse {
                description: "Successful response".to_string(),
                content: {
                    let mut content = BTreeMap::new();
                    content.insert("application/json".to_string(), OpenApiMediaType {
                        schema: response_body_schema
                    });
                    content
                },
            });
            responses
        },
    };

    spec.paths.entry(path.to_string()).or_default().post = Some(operation);

    spec
}

/// Write OpenAPI spec to JSON file.
pub fn write_openapi_json(path: &std::path::Path) -> Result<(), std::io::Error> {
    let spec = generate_openapi_spec();
    let json = serde_json::to_string_pretty(&spec)?;
    std::fs::write(path, json)
}

/// Write OpenAPI spec to YAML file.
pub fn write_openapi_yaml(path: &std::path::Path) -> Result<(), std::io::Error> {
    let spec = generate_openapi_spec();
    let yaml = serde_yaml::to_string(&spec).map_err(std::io::Error::other)?;
    std::fs::write(path, yaml)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_openapi_generation() {
        let spec = generate_openapi_spec();
        assert_eq!(spec.openapi, "3.1.0");
        assert_eq!(spec.info.title, "KVNC JSON-RPC API");
        assert!(spec.paths.contains_key("/rpc"));
        assert!(spec.components.schemas.contains_key("Transaction"));
        assert!(spec.components.schemas.contains_key("Address"));
    }

    #[test]
    fn test_openapi_json_serialization() {
        let spec = generate_openapi_spec();
        let json = serde_json::to_string_pretty(&spec).unwrap();
        // The method name appears in the request body schema as a const value
        assert!(
            json.contains("\"kvnc_getBlockByHash\""),
            "Missing kvnc_getBlockByHash"
        );
        assert!(
            json.contains("\"kvnc_sendRawTransaction\""),
            "Missing kvnc_sendRawTransaction"
        );
        assert!(
            json.contains("\"kvnc_subscribe\""),
            "Missing kvnc_subscribe"
        );
    }
}
