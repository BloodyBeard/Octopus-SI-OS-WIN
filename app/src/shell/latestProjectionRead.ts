type PendingRead<T> = {
  read: () => Promise<T>;
  resolve: (value: T | undefined) => void;
  reject: (error: unknown) => void;
  invalidated: boolean;
};

export function createLatestProjectionRead<T>() {
  let active: PendingRead<T> | null = null;
  let pending: PendingRead<T> | null = null;

  async function execute(job: PendingRead<T>): Promise<void> {
    active = job;
    try {
      const value = await job.read();
      job.resolve(job.invalidated ? undefined : value);
    } catch (error) {
      if (job.invalidated) job.resolve(undefined);
      else job.reject(error);
    } finally {
      active = null;
      const next = pending;
      pending = null;
      if (next) void execute(next);
    }
  }

  return {
    request(read: () => Promise<T>): Promise<T | undefined> {
      return new Promise((resolve, reject) => {
        const job = { read, resolve, reject, invalidated: false };
        if (active) {
          pending?.resolve(undefined);
          pending = job;
        } else {
          void execute(job);
        }
      });
    },
    invalidate(): void {
      if (active) active.invalidated = true;
      pending?.resolve(undefined);
      pending = null;
    },
  };
}
