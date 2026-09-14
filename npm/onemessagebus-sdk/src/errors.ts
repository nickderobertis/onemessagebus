// Every way a call fails, each carrying the words that explain it: the CLI's own
// refusal where the CLI refused, and a next action where the SDK could not reach it.

/** The base of every error this SDK raises. */
export class BusError extends Error {
  /** The exit code the command line gives this failure, when the bus answered one. */
  declare readonly exit: number | undefined;
  /** The document the verb printed before refusing (`ask`'s answer, `validate`'s verdict). */
  declare readonly output: unknown;

  constructor(
    message: string,
    options: { exit?: number | undefined; output?: unknown; cause?: unknown } = {},
  ) {
    super(message, options.cause === undefined ? undefined : { cause: options.cause });
    this.name = new.target.name;
    this.exit = options.exit;
    this.output = options.output;
  }
}

/** Exit 1: well-formed input whose answer is no. */
export class BusFailed extends BusError {
  constructor(message: string, output?: unknown) {
    super(message, { exit: 1, output });
  }
}

/** Exit 2: input the bus refuses. */
export class BusRefused extends BusError {
  constructor(message: string, output?: unknown) {
    super(message, { exit: 2, output });
  }
}

/** A response the generated schema rejects: the binary and this SDK disagree on the contract. */
export class ContractError extends BusError {
  /** `output` is the response the schema rejected, kept for a caller to inspect. */
  constructor(message: string, output?: unknown) {
    super(message, { output });
  }
}

/** The binary is not the version this SDK drives. */
export class VersionMismatch extends BusError {
  declare readonly expected: string;
  declare readonly actual: string;

  constructor(message: string, expected: string, actual: string) {
    super(message);
    this.expected = expected;
    this.actual = actual;
  }
}

/** The bus could not be reached: a binary that would not spawn, a socket nothing answers. */
export class TransportError extends BusError {
  /**
   * `cause` is the system error underneath (a spawn or socket failure). A transport
   * failure carries no exit code: the bus never answered, so there is no refusal.
   */
  constructor(message: string, cause?: unknown) {
    super(message, { cause });
  }
}

/** The failure for exit `code` with the CLI's refusal `message`. */
export function refusal(code: number, message: string, output?: unknown): BusError {
  if (code === 1) return new BusFailed(message, output);
  if (code === 2) return new BusRefused(message, output);
  return new BusError(message, { exit: code, output });
}
