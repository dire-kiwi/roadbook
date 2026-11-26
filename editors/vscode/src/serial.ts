// Runs operations one after another, each once the previous has finished, whatever their
// outcome. No `vscode` import, so the tests run it in plain Node.

export class Serial {
  private tail: Promise<unknown> = Promise.resolve()

  /** Runs `op` after every operation queued before it; resolves (or rejects) as it does. */
  run<T>(op: () => Promise<T>): Promise<T> {
    const result = this.tail.then(op, op)
    this.tail = result.catch(() => undefined)
    return result
  }
}
