export interface DiagnosticEvent {
  at: string;
  type: string;
  details?: unknown;
}

export interface DiagnosticsExport {
  schemaVersion: 1;
  generatedAt: string;
  context: Record<string, unknown>;
  events: DiagnosticEvent[];
}

const SENSITIVE_KEY = /secret|token|credential|password|sdp|candidate|address|ip|url/iu;
const IPV4 = /\b(?:\d{1,3}\.){3}\d{1,3}\b/gu;
const IPV6 = /\b[\da-f]{0,4}(?::[\da-f]{0,4}){2,}\b/giu;

export class DiagnosticsCollector {
  readonly #events: DiagnosticEvent[] = [];
  readonly #maximumEvents: number;

  public constructor(maximumEvents = 100) {
    this.#maximumEvents = maximumEvents;
  }

  public record(type: string, details?: unknown): void {
    this.#events.push({
      at: new Date().toISOString(),
      type: sanitizeString(type),
      details: sanitize(details),
    });
    while (this.#events.length > this.#maximumEvents) this.#events.shift();
  }

  public export(context: Record<string, unknown>): DiagnosticsExport {
    return {
      schemaVersion: 1,
      generatedAt: new Date().toISOString(),
      context: sanitize(context) as Record<string, unknown>,
      events: this.#events.map((event) => ({ ...event })),
    };
  }
}

function sanitize(value: unknown): unknown {
  if (typeof value === 'string') return sanitizeString(value);
  if (typeof value === 'number' || typeof value === 'boolean' || value === null) return value;
  if (Array.isArray(value)) return value.map(sanitize);
  if (typeof value === 'object' && value !== null) {
    const result: Record<string, unknown> = {};
    for (const [key, child] of Object.entries(value)) {
      result[key] = SENSITIVE_KEY.test(key) ? '[REDACTED]' : sanitize(child);
    }
    return result;
  }
  return undefined;
}

function sanitizeString(value: string): string {
  return value
    .replace(/#.*$/u, '#[REDACTED]')
    .replace(IPV4, '[IP REDACTED]')
    .replace(IPV6, '[IP REDACTED]');
}
