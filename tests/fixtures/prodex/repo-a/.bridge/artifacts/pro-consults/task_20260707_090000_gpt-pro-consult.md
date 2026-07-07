Use exponential backoff with full jitter. Fixed intervals synchronize retries
across clients and produce thundering herds; full jitter spreads them.

Cap the backoff at 30s and budget total retry time, not attempt count.
