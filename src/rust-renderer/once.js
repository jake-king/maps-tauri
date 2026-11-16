// A lazy async load that runs at most once at a time. Callers that arrive while it is in flight share
// the same promise (guarding on the finished result instead lets a second caller start a second load);
// a failed load is dropped so the next call tries again instead of failing forever.
export function once(load) {
  let flight = null;
  return () => {
    flight ??= Promise.resolve()
      .then(load)
      .catch((e) => {
        flight = null;
        throw e;
      });
    return flight;
  };
}
