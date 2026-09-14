// @onemessagebus/sdk: the typed client, its transports, and the generated contract.
export {
  type Binary,
  type ClientConfig,
  pinnedVersion,
  resolveBinary,
  verifyVersion,
} from "./binary.js";
export {
  type Abandoned,
  type Answer,
  type Claimed,
  Client,
  type ClientOptions,
  type MessageTypeLike,
  type Predicate,
  type QueueStatus,
  type AsJson,
  type AsText,
  type Refused,
  type Reply,
  SchemaApi,
  type Timeout,
} from "./client.js";
export {
  BusError,
  BusFailed,
  BusRefused,
  ContractError,
  TransportError,
  VersionMismatch,
} from "./errors.js";
export * from "./generated/index.js";
export {
  defineMessage,
  type JsonSchemaDocument,
  type MessageDefinition,
  type MessageType,
} from "./message.js";
export {
  type Args,
  CliTransport,
  ResidentTransport,
  type ResidentTransportOptions,
  renderArgv,
  type Transport,
} from "./transport.js";
export { CLI_VERSION, SDK_VERSION } from "./version.js";
