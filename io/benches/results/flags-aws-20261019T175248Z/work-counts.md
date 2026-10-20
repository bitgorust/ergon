# Work behind the percentiles — run flags-aws-20261019T175248Z

Extracted from the run's generated `tables.md` (gitignored by policy) so the
completeness counts cited in `BENCHMARK.md` are checkable from the repository.
Counts are the run's own account, reconciled against `records.bin` before any
warm-up is dropped. Every percentile in `BENCHMARK.md` is over `completed`
alone, after the warm-up cut, so `n` is smaller still.

`scheduled` `issued` `completed` `late` `rejected` `censored`

## HTTP round trip, http-epoll-spin-p1-r10000

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | 600001 | 600001 | 600001 | 93 | 0 | 0 |
| f1-irqlocal | 600001 | 600001 | 600001 | 94 | 0 | 0 |
| f2-irqlocal | 600001 | 600001 | 600001 | 89 | 0 | 0 |
| f2-irqcore | 600001 | 600001 | 600001 | 93 | 0 | 0 |

## HTTP round trip, http-epoll-spin-p20-r10000

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | 600001 | 600001 | 600001 | 94 | 0 | 0 |
| f1-irqlocal | 600001 | 600001 | 600001 | 96 | 0 | 0 |
| f2-irqlocal | 600001 | 600001 | 600001 | 122 | 0 | 0 |
| f2-irqcore | 600001 | 600001 | 600001 | 102 | 0 | 0 |

## HTTP round trip, http-recv-busy-p1-r10000

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | 600001 | 600001 | 600001 | 87 | 0 | 0 |
| f2-irqcore | 600001 | 600001 | 600001 | 94 | 0 | 0 |

## HTTP round trip, http-recv-busy-p20-r10000

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | 600001 | 600001 | 600001 | 142 | 0 | 0 |
| f2-irqcore | 600001 | 600001 | 600001 | 196 | 0 | 0 |

## HTTP round trip, http-recv-busy-pq-p1-r10000

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | 600001 | 600001 | 600001 | 88 | 0 | 0 |
| f2-irqcore | 600001 | 600001 | 600001 | 93 | 0 | 0 |

## HTTP round trip, http-recv-busy-pq-p20-r10000

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | 600001 | 600001 | 600001 | 109 | 0 | 0 |
| f2-irqcore | 600001 | 600001 | 600001 | 135 | 0 | 0 |

## HTTP round trip, http-uring-spin-p1-r10000

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | 600001 | 600001 | 600001 | 89 | 0 | 0 |
| f1-irqlocal | 600001 | 600001 | 600001 | 91 | 0 | 0 |
| f2-irqlocal | 600001 | 600001 | 600001 | 92 | 0 | 0 |
| f2-irqcore | 600001 | 600001 | 600001 | 97 | 0 | 0 |

## HTTP round trip, http-uring-spin-p20-r10000

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | 600001 | 600001 | 600001 | 97 | 0 | 0 |
| f1-irqlocal | 600001 | 600001 | 600001 | 116 | 0 | 0 |
| f2-irqlocal | 600001 | 600001 | 600001 | 95 | 0 | 0 |
| f2-irqcore | 600001 | 600001 | 600001 | 140 | 0 | 0 |

## HTTP round trip, http-uring-sqpoll-p1-r10000

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | 600001 | 600001 | 600001 | 88 | 0 | 0 |
| f2-irqcore | 600001 | 600001 | 600001 | 93 | 0 | 0 |

## HTTP round trip, http-uring-sqpoll-p20-r10000

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | 600001 | 600001 | 600001 | 106 | 0 | 0 |
| f2-irqcore | 600001 | 600001 | 600001 | 99 | 0 | 0 |

## Market-data burst, kernel arrival → decoded (every message), md-epoll-spin-c1

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | not yet measured | not yet measured | 3056298 | not yet measured | not yet measured | 0 |
| f1-irqlocal | not yet measured | not yet measured | 3056666 | not yet measured | not yet measured | 0 |
| f2-irqlocal | not yet measured | not yet measured | 3056795 | not yet measured | not yet measured | 0 |
| f2-irqcore | not yet measured | not yet measured | 3056397 | not yet measured | not yet measured | 0 |

## Market-data burst, kernel arrival → decoded (every message), md-epoll-spin-c16

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | not yet measured | not yet measured | 3056288 | not yet measured | not yet measured | 0 |
| f1-irqlocal | not yet measured | not yet measured | 3056993 | not yet measured | not yet measured | 0 |
| f2-irqlocal | not yet measured | not yet measured | 3057207 | not yet measured | not yet measured | 0 |
| f2-irqcore | not yet measured | not yet measured | 3056388 | not yet measured | not yet measured | 0 |

## Market-data burst, kernel arrival → decoded (every message), md-epoll-spin-c256

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | not yet measured | not yet measured | 3056286 | not yet measured | not yet measured | 0 |
| f1-irqlocal | not yet measured | not yet measured | 3057197 | not yet measured | not yet measured | 0 |
| f2-irqlocal | not yet measured | not yet measured | 3057694 | not yet measured | not yet measured | 0 |
| f2-irqcore | not yet measured | not yet measured | 3056806 | not yet measured | not yet measured | 0 |

## Market-data burst, kernel arrival → decoded (every message), md-epoll-spin-c4

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | not yet measured | not yet measured | 3056352 | not yet measured | not yet measured | 0 |
| f1-irqlocal | not yet measured | not yet measured | 3056901 | not yet measured | not yet measured | 0 |
| f2-irqlocal | not yet measured | not yet measured | 3056902 | not yet measured | not yet measured | 0 |
| f2-irqcore | not yet measured | not yet measured | 3056401 | not yet measured | not yet measured | 0 |

## Market-data burst, kernel arrival → decoded (every message), md-epoll-spin-c64

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | not yet measured | not yet measured | 3056287 | not yet measured | not yet measured | 0 |
| f1-irqlocal | not yet measured | not yet measured | 3057099 | not yet measured | not yet measured | 0 |
| f2-irqlocal | not yet measured | not yet measured | 3057388 | not yet measured | not yet measured | 0 |
| f2-irqcore | not yet measured | not yet measured | 3056494 | not yet measured | not yet measured | 0 |

## Market-data burst, kernel arrival → decoded (every message), md-mio-c1

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | not yet measured | not yet measured | 3056298 | not yet measured | not yet measured | 0 |
| f1-irqlocal | not yet measured | not yet measured | 3056373 | not yet measured | not yet measured | 0 |
| f2-irqlocal | not yet measured | not yet measured | 3056457 | not yet measured | not yet measured | 0 |
| f2-irqcore | not yet measured | not yet measured | 3056477 | not yet measured | not yet measured | 0 |

## Market-data burst, kernel arrival → decoded (every message), md-mio-c16

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | not yet measured | not yet measured | 3056266 | not yet measured | not yet measured | 0 |
| f1-irqlocal | not yet measured | not yet measured | 3056972 | not yet measured | not yet measured | 0 |
| f2-irqlocal | not yet measured | not yet measured | 3057098 | not yet measured | not yet measured | 0 |
| f2-irqcore | not yet measured | not yet measured | 3056401 | not yet measured | not yet measured | 0 |

## Market-data burst, kernel arrival → decoded (every message), md-mio-c256

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | not yet measured | not yet measured | 3056276 | not yet measured | not yet measured | 0 |
| f1-irqlocal | not yet measured | not yet measured | 3057169 | not yet measured | not yet measured | 0 |
| f2-irqlocal | not yet measured | not yet measured | 3057281 | not yet measured | not yet measured | 0 |
| f2-irqcore | not yet measured | not yet measured | 3056602 | not yet measured | not yet measured | 0 |

## Market-data burst, kernel arrival → decoded (every message), md-mio-c4

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | not yet measured | not yet measured | 3056361 | not yet measured | not yet measured | 0 |
| f1-irqlocal | not yet measured | not yet measured | 3056884 | not yet measured | not yet measured | 0 |
| f2-irqlocal | not yet measured | not yet measured | 3056967 | not yet measured | not yet measured | 0 |
| f2-irqcore | not yet measured | not yet measured | 3056479 | not yet measured | not yet measured | 0 |

## Market-data burst, kernel arrival → decoded (every message), md-mio-c64

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | not yet measured | not yet measured | 3056288 | not yet measured | not yet measured | 0 |
| f1-irqlocal | not yet measured | not yet measured | 3057105 | not yet measured | not yet measured | 0 |
| f2-irqlocal | not yet measured | not yet measured | 3057206 | not yet measured | not yet measured | 0 |
| f2-irqcore | not yet measured | not yet measured | 3056479 | not yet measured | not yet measured | 0 |

## Market-data burst, kernel arrival → decoded (every message), md-recv-busy-c1

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | not yet measured | not yet measured | 3056288 | not yet measured | not yet measured | 0 |
| f2-irqcore | not yet measured | not yet measured | 3056390 | not yet measured | not yet measured | 0 |

## Market-data burst, kernel arrival → decoded (every message), md-recv-busy-c16

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | not yet measured | not yet measured | 3056361 | not yet measured | not yet measured | 0 |
| f2-irqcore | not yet measured | not yet measured | 3056470 | not yet measured | not yet measured | 0 |

## Market-data burst, kernel arrival → decoded (every message), md-recv-busy-c256

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | not yet measured | not yet measured | 3056368 | not yet measured | not yet measured | 0 |
| f2-irqcore | not yet measured | not yet measured | 3057204 | not yet measured | not yet measured | 0 |

## Market-data burst, kernel arrival → decoded (every message), md-recv-busy-c4

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | not yet measured | not yet measured | 3056353 | not yet measured | not yet measured | 0 |
| f2-irqcore | not yet measured | not yet measured | 3056403 | not yet measured | not yet measured | 0 |

## Market-data burst, kernel arrival → decoded (every message), md-recv-busy-c64

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | not yet measured | not yet measured | 3056373 | not yet measured | not yet measured | 0 |
| f2-irqcore | not yet measured | not yet measured | 3056401 | not yet measured | not yet measured | 0 |

## Market-data burst, kernel arrival → decoded (every message), md-recv-busy-pq-c1

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | not yet measured | not yet measured | 3056288 | not yet measured | not yet measured | 0 |
| f2-irqcore | not yet measured | not yet measured | 3056396 | not yet measured | not yet measured | 0 |

## Market-data burst, kernel arrival → decoded (every message), md-recv-busy-pq-c16

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | not yet measured | not yet measured | 3056295 | not yet measured | not yet measured | 0 |
| f2-irqcore | not yet measured | not yet measured | 3056468 | not yet measured | not yet measured | 0 |

## Market-data burst, kernel arrival → decoded (every message), md-recv-busy-pq-c256

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | not yet measured | not yet measured | 3056298 | not yet measured | not yet measured | 0 |
| f2-irqcore | not yet measured | not yet measured | 3056691 | not yet measured | not yet measured | 0 |

## Market-data burst, kernel arrival → decoded (every message), md-recv-busy-pq-c4

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | not yet measured | not yet measured | 3056354 | not yet measured | not yet measured | 0 |
| f2-irqcore | not yet measured | not yet measured | 3056457 | not yet measured | not yet measured | 0 |

## Market-data burst, kernel arrival → decoded (every message), md-recv-busy-pq-c64

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | not yet measured | not yet measured | 3056298 | not yet measured | not yet measured | 0 |
| f2-irqcore | not yet measured | not yet measured | 3056401 | not yet measured | not yet measured | 0 |

## Market-data burst, kernel arrival → decoded (every message), md-uring-spin-c1

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | not yet measured | not yet measured | 3056271 | not yet measured | not yet measured | 0 |
| f1-irqlocal | not yet measured | not yet measured | 3056768 | not yet measured | not yet measured | 0 |
| f2-irqlocal | not yet measured | not yet measured | 3056764 | not yet measured | not yet measured | 0 |
| f2-irqcore | not yet measured | not yet measured | 3056401 | not yet measured | not yet measured | 0 |

## Market-data burst, kernel arrival → decoded (every message), md-uring-spin-c16

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | not yet measured | not yet measured | 3056276 | not yet measured | not yet measured | 0 |
| f1-irqlocal | not yet measured | not yet measured | 3057007 | not yet measured | not yet measured | 0 |
| f2-irqlocal | not yet measured | not yet measured | 3057194 | not yet measured | not yet measured | 0 |
| f2-irqcore | not yet measured | not yet measured | 3056399 | not yet measured | not yet measured | 0 |

## Market-data burst, kernel arrival → decoded (every message), md-uring-spin-c256

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | not yet measured | not yet measured | 3056283 | not yet measured | not yet measured | 0 |
| f1-irqlocal | not yet measured | not yet measured | 3057112 | not yet measured | not yet measured | 0 |
| f2-irqlocal | not yet measured | not yet measured | 3057271 | not yet measured | not yet measured | 0 |
| f2-irqcore | not yet measured | not yet measured | 3056711 | not yet measured | not yet measured | 0 |

## Market-data burst, kernel arrival → decoded (every message), md-uring-spin-c4

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | not yet measured | not yet measured | 3056266 | not yet measured | not yet measured | 0 |
| f1-irqlocal | not yet measured | not yet measured | 3056878 | not yet measured | not yet measured | 0 |
| f2-irqlocal | not yet measured | not yet measured | 3056969 | not yet measured | not yet measured | 0 |
| f2-irqcore | not yet measured | not yet measured | 3056401 | not yet measured | not yet measured | 0 |

## Market-data burst, kernel arrival → decoded (every message), md-uring-spin-c64

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | not yet measured | not yet measured | 3056288 | not yet measured | not yet measured | 0 |
| f1-irqlocal | not yet measured | not yet measured | 3057010 | not yet measured | not yet measured | 0 |
| f2-irqlocal | not yet measured | not yet measured | 3057611 | not yet measured | not yet measured | 0 |
| f2-irqcore | not yet measured | not yet measured | 3056388 | not yet measured | not yet measured | 0 |

## Market-data burst, kernel arrival → decoded (every message), md-uring-sqpoll-c1

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | not yet measured | not yet measured | 3056288 | not yet measured | not yet measured | 0 |
| f2-irqcore | not yet measured | not yet measured | 3056494 | not yet measured | not yet measured | 0 |

## Market-data burst, kernel arrival → decoded (every message), md-uring-sqpoll-c16

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | not yet measured | not yet measured | 3056288 | not yet measured | not yet measured | 0 |
| f2-irqcore | not yet measured | not yet measured | 3056394 | not yet measured | not yet measured | 0 |

## Market-data burst, kernel arrival → decoded (every message), md-uring-sqpoll-c256

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | not yet measured | not yet measured | 3056272 | not yet measured | not yet measured | 0 |
| f2-irqcore | not yet measured | not yet measured | 3056502 | not yet measured | not yet measured | 0 |

## Market-data burst, kernel arrival → decoded (every message), md-uring-sqpoll-c4

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | not yet measured | not yet measured | 3056364 | not yet measured | not yet measured | 0 |
| f2-irqcore | not yet measured | not yet measured | 3056474 | not yet measured | not yet measured | 0 |

## Market-data burst, kernel arrival → decoded (every message), md-uring-sqpoll-c64

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | not yet measured | not yet measured | 3056287 | not yet measured | not yet measured | 0 |
| f2-irqcore | not yet measured | not yet measured | 3056399 | not yet measured | not yet measured | 0 |

## WebSocket order round trip with feed connections on the loop, rt-epoll-spin-n1-r10000

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | 600001 | 600000 | 600000 | 0 | 0 | 0 |
| f1-irqlocal | 600001 | 600000 | 600000 | 0 | 0 | 0 |
| f2-irqlocal | 600001 | 600000 | 600000 | 1 | 0 | 0 |
| f2-irqcore | 600001 | 600000 | 600000 | 0 | 0 | 0 |

## WebSocket order round trip with feed connections on the loop, rt-epoll-spin-n16-r10000

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | 600001 | 600000 | 600000 | 6 | 0 | 0 |
| f1-irqlocal | 600001 | 600000 | 600000 | 13 | 0 | 0 |
| f2-irqlocal | 600001 | 600000 | 600000 | 7 | 0 | 0 |
| f2-irqcore | 600001 | 600000 | 600000 | 2 | 0 | 0 |

## WebSocket order round trip with feed connections on the loop, rt-epoll-spin-n4-r10000

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | 600001 | 600000 | 600000 | 29 | 0 | 0 |
| f1-irqlocal | 600001 | 600000 | 600000 | 5 | 0 | 0 |
| f2-irqlocal | 600001 | 600000 | 600000 | 4 | 0 | 0 |
| f2-irqcore | 600001 | 600000 | 600000 | 2 | 0 | 0 |

## WebSocket order round trip with feed connections on the loop, rt-epoll-spin-n64-r10000

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | 600001 | 600000 | 600000 | 3 | 0 | 0 |
| f1-irqlocal | 600001 | 600000 | 600000 | 10 | 0 | 0 |
| f2-irqlocal | 600001 | 600000 | 600000 | 21 | 0 | 0 |
| f2-irqcore | 600001 | 600000 | 600000 | 4 | 0 | 0 |

## WebSocket order round trip with feed connections on the loop, rt-mio-n1-r10000

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | 600157 | 600000 | 600000 | 0 | 0 | 0 |
| f1-irqlocal | 600161 | 600000 | 600000 | 0 | 0 | 0 |
| f2-irqlocal | 600161 | 600000 | 600000 | 0 | 0 | 0 |
| f2-irqcore | 600155 | 600000 | 600000 | 0 | 0 | 0 |

## WebSocket order round trip with feed connections on the loop, rt-mio-n16-r10000

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | 600154 | 599997 | 599997 | 0 | 0 | 0 |
| f1-irqlocal | 600247 | 599995 | 599995 | 47 | 0 | 0 |
| f2-irqlocal | 600257 | 599995 | 599995 | 27 | 0 | 0 |
| f2-irqcore | 600158 | 599997 | 599996 | 0 | 0 | 1 |

## WebSocket order round trip with feed connections on the loop, rt-mio-n4-r10000

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | 600156 | 599997 | 599997 | 0 | 0 | 0 |
| f1-irqlocal | 600216 | 599996 | 599996 | 1 | 0 | 0 |
| f2-irqlocal | 600216 | 599996 | 599996 | 4 | 0 | 0 |
| f2-irqcore | 600165 | 599997 | 599997 | 1 | 0 | 0 |

## WebSocket order round trip with feed connections on the loop, rt-mio-n64-r10000

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | 600154 | 599996 | 599996 | 0 | 0 | 0 |
| f1-irqlocal | 600261 | 599995 | 599994 | 4 | 0 | 1 |
| f2-irqlocal | 600265 | 599994 | 599994 | 4 | 0 | 0 |
| f2-irqcore | 600156 | 599996 | 599996 | 24 | 0 | 0 |

## WebSocket order round trip with feed connections on the loop, rt-recv-busy-n1-r10000

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | 600001 | 600000 | 600000 | 0 | 0 | 0 |
| f2-irqcore | 600001 | 600000 | 600000 | 27 | 0 | 0 |

## WebSocket order round trip with feed connections on the loop, rt-recv-busy-n16-r10000

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | 600005 | 600004 | 600004 | 5 | 0 | 0 |
| f2-irqcore | 600006 | 600005 | 600005 | 6 | 0 | 0 |

## WebSocket order round trip with feed connections on the loop, rt-recv-busy-n4-r10000

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | 600002 | 600001 | 600001 | 3 | 0 | 0 |
| f2-irqcore | 600001 | 600001 | 600000 | 2 | 0 | 1 |

## WebSocket order round trip with feed connections on the loop, rt-recv-busy-n64-r10000

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | 600002 | 501710 | 501709 | 501709 | 0 | 1 |
| f2-irqcore | 600001 | 501971 | 501970 | 501970 | 0 | 1 |

## WebSocket order round trip with feed connections on the loop, rt-recv-busy-pq-n1-r10000

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | 600001 | 600001 | 600000 | 0 | 0 | 1 |
| f2-irqcore | 600001 | 600000 | 600000 | 1 | 0 | 0 |

## WebSocket order round trip with feed connections on the loop, rt-recv-busy-pq-n16-r10000

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | 600001 | 600000 | 600000 | 3 | 0 | 0 |
| f2-irqcore | 600003 | 600002 | 600002 | 3 | 0 | 0 |

## WebSocket order round trip with feed connections on the loop, rt-recv-busy-pq-n4-r10000

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | 600001 | 600000 | 600000 | 3 | 0 | 0 |
| f2-irqcore | 600001 | 600000 | 600000 | 3 | 0 | 0 |

## WebSocket order round trip with feed connections on the loop, rt-recv-busy-pq-n64-r10000

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | 600004 | 600002 | 600002 | 7 | 0 | 0 |
| f2-irqcore | 600003 | 600002 | 600001 | 7 | 0 | 1 |

## WebSocket order round trip with feed connections on the loop, rt-uring-spin-n1-r10000

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | 600001 | 600000 | 600000 | 1 | 0 | 0 |
| f1-irqlocal | 600001 | 600000 | 600000 | 0 | 0 | 0 |
| f2-irqlocal | 600001 | 600000 | 600000 | 1 | 0 | 0 |
| f2-irqcore | 600001 | 600000 | 600000 | 0 | 0 | 0 |

## WebSocket order round trip with feed connections on the loop, rt-uring-spin-n16-r10000

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | 600001 | 600000 | 600000 | 2 | 0 | 0 |
| f1-irqlocal | 600001 | 600000 | 600000 | 6 | 0 | 0 |
| f2-irqlocal | 600001 | 600000 | 600000 | 5 | 0 | 0 |
| f2-irqcore | 600001 | 600000 | 600000 | 2 | 0 | 0 |

## WebSocket order round trip with feed connections on the loop, rt-uring-spin-n4-r10000

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | 600001 | 600000 | 600000 | 1 | 0 | 0 |
| f1-irqlocal | 600001 | 600000 | 600000 | 7 | 0 | 0 |
| f2-irqlocal | 600001 | 600000 | 600000 | 4 | 0 | 0 |
| f2-irqcore | 600001 | 600000 | 600000 | 2 | 0 | 0 |

## WebSocket order round trip with feed connections on the loop, rt-uring-spin-n64-r10000

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | 600001 | 600000 | 600000 | 3 | 0 | 0 |
| f1-irqlocal | 600001 | 600000 | 600000 | 32 | 0 | 0 |
| f2-irqlocal | 600001 | 600000 | 600000 | 11 | 0 | 0 |
| f2-irqcore | 600001 | 600000 | 600000 | 4 | 0 | 0 |

## WebSocket order round trip with feed connections on the loop, rt-uring-sqpoll-n1-r10000

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | 600001 | 600000 | 600000 | 23 | 0 | 0 |
| f2-irqcore | 600001 | 600000 | 600000 | 0 | 0 | 0 |

## WebSocket order round trip with feed connections on the loop, rt-uring-sqpoll-n16-r10000

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | 600001 | 600000 | 600000 | 3 | 0 | 0 |
| f2-irqcore | 600001 | 600000 | 600000 | 2 | 0 | 0 |

## WebSocket order round trip with feed connections on the loop, rt-uring-sqpoll-n4-r10000

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | 600001 | 600000 | 600000 | 1 | 0 | 0 |
| f2-irqcore | 600001 | 600000 | 600000 | 1 | 0 | 0 |

## WebSocket order round trip with feed connections on the loop, rt-uring-sqpoll-n64-r10000

| | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore | 600001 | 600000 | 600000 | 3 | 0 | 0 |
| f2-irqcore | 600001 | 600000 | 600000 | 3 | 0 | 0 |

## f1-irqcore/md-epoll-spin-c1: lab, 60 s (first 2 s dropped), 1 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore/md-epoll-spin-c1 | not yet measured | not yet measured | 3056298 | not yet measured | not yet measured | 0 |

## f1-irqcore/md-epoll-spin-c16: lab, 60 s (first 2 s dropped), 16 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore/md-epoll-spin-c16 | not yet measured | not yet measured | 3056288 | not yet measured | not yet measured | 0 |

## f1-irqcore/md-epoll-spin-c256: lab, 60 s (first 2 s dropped), 256 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore/md-epoll-spin-c256 | not yet measured | not yet measured | 3056286 | not yet measured | not yet measured | 0 |

## f1-irqcore/md-epoll-spin-c4: lab, 60 s (first 2 s dropped), 4 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore/md-epoll-spin-c4 | not yet measured | not yet measured | 3056352 | not yet measured | not yet measured | 0 |

## f1-irqcore/md-epoll-spin-c64: lab, 60 s (first 2 s dropped), 64 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore/md-epoll-spin-c64 | not yet measured | not yet measured | 3056287 | not yet measured | not yet measured | 0 |

## f1-irqcore/md-mio-c1: lab, 60 s (first 2 s dropped),  connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore/md-mio-c1 | not yet measured | not yet measured | 3056298 | not yet measured | not yet measured | 0 |

## f1-irqcore/md-mio-c16: lab, 60 s (first 2 s dropped),  connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore/md-mio-c16 | not yet measured | not yet measured | 3056266 | not yet measured | not yet measured | 0 |

## f1-irqcore/md-mio-c256: lab, 60 s (first 2 s dropped),  connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore/md-mio-c256 | not yet measured | not yet measured | 3056276 | not yet measured | not yet measured | 0 |

## f1-irqcore/md-mio-c4: lab, 60 s (first 2 s dropped),  connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore/md-mio-c4 | not yet measured | not yet measured | 3056361 | not yet measured | not yet measured | 0 |

## f1-irqcore/md-mio-c64: lab, 60 s (first 2 s dropped),  connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore/md-mio-c64 | not yet measured | not yet measured | 3056288 | not yet measured | not yet measured | 0 |

## f1-irqcore/md-recv-busy-c1: lab, 60 s (first 2 s dropped), 1 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore/md-recv-busy-c1 | not yet measured | not yet measured | 3056288 | not yet measured | not yet measured | 0 |

## f1-irqcore/md-recv-busy-c16: lab, 60 s (first 2 s dropped), 16 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore/md-recv-busy-c16 | not yet measured | not yet measured | 3056361 | not yet measured | not yet measured | 0 |

## f1-irqcore/md-recv-busy-c256: lab, 60 s (first 2 s dropped), 256 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore/md-recv-busy-c256 | not yet measured | not yet measured | 3056368 | not yet measured | not yet measured | 0 |

## f1-irqcore/md-recv-busy-c4: lab, 60 s (first 2 s dropped), 4 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore/md-recv-busy-c4 | not yet measured | not yet measured | 3056353 | not yet measured | not yet measured | 0 |

## f1-irqcore/md-recv-busy-c64: lab, 60 s (first 2 s dropped), 64 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore/md-recv-busy-c64 | not yet measured | not yet measured | 3056373 | not yet measured | not yet measured | 0 |

## f1-irqcore/md-recv-busy-pq-c1: lab, 60 s (first 2 s dropped), 1 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore/md-recv-busy-pq-c1 | not yet measured | not yet measured | 3056288 | not yet measured | not yet measured | 0 |

## f1-irqcore/md-recv-busy-pq-c16: lab, 60 s (first 2 s dropped), 16 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore/md-recv-busy-pq-c16 | not yet measured | not yet measured | 3056295 | not yet measured | not yet measured | 0 |

## f1-irqcore/md-recv-busy-pq-c256: lab, 60 s (first 2 s dropped), 256 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore/md-recv-busy-pq-c256 | not yet measured | not yet measured | 3056298 | not yet measured | not yet measured | 0 |

## f1-irqcore/md-recv-busy-pq-c4: lab, 60 s (first 2 s dropped), 4 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore/md-recv-busy-pq-c4 | not yet measured | not yet measured | 3056354 | not yet measured | not yet measured | 0 |

## f1-irqcore/md-recv-busy-pq-c64: lab, 60 s (first 2 s dropped), 64 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore/md-recv-busy-pq-c64 | not yet measured | not yet measured | 3056298 | not yet measured | not yet measured | 0 |

## f1-irqcore/md-uring-spin-c1: lab, 60 s (first 2 s dropped), 1 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore/md-uring-spin-c1 | not yet measured | not yet measured | 3056271 | not yet measured | not yet measured | 0 |

## f1-irqcore/md-uring-spin-c16: lab, 60 s (first 2 s dropped), 16 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore/md-uring-spin-c16 | not yet measured | not yet measured | 3056276 | not yet measured | not yet measured | 0 |

## f1-irqcore/md-uring-spin-c256: lab, 60 s (first 2 s dropped), 256 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore/md-uring-spin-c256 | not yet measured | not yet measured | 3056283 | not yet measured | not yet measured | 0 |

## f1-irqcore/md-uring-spin-c4: lab, 60 s (first 2 s dropped), 4 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore/md-uring-spin-c4 | not yet measured | not yet measured | 3056266 | not yet measured | not yet measured | 0 |

## f1-irqcore/md-uring-spin-c64: lab, 60 s (first 2 s dropped), 64 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore/md-uring-spin-c64 | not yet measured | not yet measured | 3056288 | not yet measured | not yet measured | 0 |

## f1-irqcore/md-uring-sqpoll-c1: lab, 60 s (first 2 s dropped), 1 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore/md-uring-sqpoll-c1 | not yet measured | not yet measured | 3056288 | not yet measured | not yet measured | 0 |

## f1-irqcore/md-uring-sqpoll-c16: lab, 60 s (first 2 s dropped), 16 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore/md-uring-sqpoll-c16 | not yet measured | not yet measured | 3056288 | not yet measured | not yet measured | 0 |

## f1-irqcore/md-uring-sqpoll-c256: lab, 60 s (first 2 s dropped), 256 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore/md-uring-sqpoll-c256 | not yet measured | not yet measured | 3056272 | not yet measured | not yet measured | 0 |

## f1-irqcore/md-uring-sqpoll-c4: lab, 60 s (first 2 s dropped), 4 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore/md-uring-sqpoll-c4 | not yet measured | not yet measured | 3056364 | not yet measured | not yet measured | 0 |

## f1-irqcore/md-uring-sqpoll-c64: lab, 60 s (first 2 s dropped), 64 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqcore/md-uring-sqpoll-c64 | not yet measured | not yet measured | 3056287 | not yet measured | not yet measured | 0 |

## f1-irqlocal/md-epoll-spin-c1: lab, 60 s (first 2 s dropped), 1 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqlocal/md-epoll-spin-c1 | not yet measured | not yet measured | 3056666 | not yet measured | not yet measured | 0 |

## f1-irqlocal/md-epoll-spin-c16: lab, 60 s (first 2 s dropped), 16 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqlocal/md-epoll-spin-c16 | not yet measured | not yet measured | 3056993 | not yet measured | not yet measured | 0 |

## f1-irqlocal/md-epoll-spin-c256: lab, 60 s (first 2 s dropped), 256 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqlocal/md-epoll-spin-c256 | not yet measured | not yet measured | 3057197 | not yet measured | not yet measured | 0 |

## f1-irqlocal/md-epoll-spin-c4: lab, 60 s (first 2 s dropped), 4 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqlocal/md-epoll-spin-c4 | not yet measured | not yet measured | 3056901 | not yet measured | not yet measured | 0 |

## f1-irqlocal/md-epoll-spin-c64: lab, 60 s (first 2 s dropped), 64 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqlocal/md-epoll-spin-c64 | not yet measured | not yet measured | 3057099 | not yet measured | not yet measured | 0 |

## f1-irqlocal/md-mio-c1: lab, 60 s (first 2 s dropped),  connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqlocal/md-mio-c1 | not yet measured | not yet measured | 3056373 | not yet measured | not yet measured | 0 |

## f1-irqlocal/md-mio-c16: lab, 60 s (first 2 s dropped),  connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqlocal/md-mio-c16 | not yet measured | not yet measured | 3056972 | not yet measured | not yet measured | 0 |

## f1-irqlocal/md-mio-c256: lab, 60 s (first 2 s dropped),  connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqlocal/md-mio-c256 | not yet measured | not yet measured | 3057169 | not yet measured | not yet measured | 0 |

## f1-irqlocal/md-mio-c4: lab, 60 s (first 2 s dropped),  connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqlocal/md-mio-c4 | not yet measured | not yet measured | 3056884 | not yet measured | not yet measured | 0 |

## f1-irqlocal/md-mio-c64: lab, 60 s (first 2 s dropped),  connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqlocal/md-mio-c64 | not yet measured | not yet measured | 3057105 | not yet measured | not yet measured | 0 |

## f1-irqlocal/md-uring-spin-c1: lab, 60 s (first 2 s dropped), 1 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqlocal/md-uring-spin-c1 | not yet measured | not yet measured | 3056768 | not yet measured | not yet measured | 0 |

## f1-irqlocal/md-uring-spin-c16: lab, 60 s (first 2 s dropped), 16 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqlocal/md-uring-spin-c16 | not yet measured | not yet measured | 3057007 | not yet measured | not yet measured | 0 |

## f1-irqlocal/md-uring-spin-c256: lab, 60 s (first 2 s dropped), 256 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqlocal/md-uring-spin-c256 | not yet measured | not yet measured | 3057112 | not yet measured | not yet measured | 0 |

## f1-irqlocal/md-uring-spin-c4: lab, 60 s (first 2 s dropped), 4 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqlocal/md-uring-spin-c4 | not yet measured | not yet measured | 3056878 | not yet measured | not yet measured | 0 |

## f1-irqlocal/md-uring-spin-c64: lab, 60 s (first 2 s dropped), 64 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f1-irqlocal/md-uring-spin-c64 | not yet measured | not yet measured | 3057010 | not yet measured | not yet measured | 0 |

## f2-irqlocal/md-epoll-spin-c1: lab, 60 s (first 2 s dropped), 1 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f2-irqlocal/md-epoll-spin-c1 | not yet measured | not yet measured | 3056795 | not yet measured | not yet measured | 0 |

## f2-irqlocal/md-epoll-spin-c16: lab, 60 s (first 2 s dropped), 16 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f2-irqlocal/md-epoll-spin-c16 | not yet measured | not yet measured | 3057207 | not yet measured | not yet measured | 0 |

## f2-irqlocal/md-epoll-spin-c256: lab, 60 s (first 2 s dropped), 256 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f2-irqlocal/md-epoll-spin-c256 | not yet measured | not yet measured | 3057694 | not yet measured | not yet measured | 0 |

## f2-irqlocal/md-epoll-spin-c4: lab, 60 s (first 2 s dropped), 4 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f2-irqlocal/md-epoll-spin-c4 | not yet measured | not yet measured | 3056902 | not yet measured | not yet measured | 0 |

## f2-irqlocal/md-epoll-spin-c64: lab, 60 s (first 2 s dropped), 64 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f2-irqlocal/md-epoll-spin-c64 | not yet measured | not yet measured | 3057388 | not yet measured | not yet measured | 0 |

## f2-irqlocal/md-mio-c1: lab, 60 s (first 2 s dropped),  connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f2-irqlocal/md-mio-c1 | not yet measured | not yet measured | 3056457 | not yet measured | not yet measured | 0 |

## f2-irqlocal/md-mio-c16: lab, 60 s (first 2 s dropped),  connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f2-irqlocal/md-mio-c16 | not yet measured | not yet measured | 3057098 | not yet measured | not yet measured | 0 |

## f2-irqlocal/md-mio-c256: lab, 60 s (first 2 s dropped),  connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f2-irqlocal/md-mio-c256 | not yet measured | not yet measured | 3057281 | not yet measured | not yet measured | 0 |

## f2-irqlocal/md-mio-c4: lab, 60 s (first 2 s dropped),  connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f2-irqlocal/md-mio-c4 | not yet measured | not yet measured | 3056967 | not yet measured | not yet measured | 0 |

## f2-irqlocal/md-mio-c64: lab, 60 s (first 2 s dropped),  connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f2-irqlocal/md-mio-c64 | not yet measured | not yet measured | 3057206 | not yet measured | not yet measured | 0 |

## f2-irqlocal/md-uring-spin-c1: lab, 60 s (first 2 s dropped), 1 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f2-irqlocal/md-uring-spin-c1 | not yet measured | not yet measured | 3056764 | not yet measured | not yet measured | 0 |

## f2-irqlocal/md-uring-spin-c16: lab, 60 s (first 2 s dropped), 16 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f2-irqlocal/md-uring-spin-c16 | not yet measured | not yet measured | 3057194 | not yet measured | not yet measured | 0 |

## f2-irqlocal/md-uring-spin-c256: lab, 60 s (first 2 s dropped), 256 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f2-irqlocal/md-uring-spin-c256 | not yet measured | not yet measured | 3057271 | not yet measured | not yet measured | 0 |

## f2-irqlocal/md-uring-spin-c4: lab, 60 s (first 2 s dropped), 4 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f2-irqlocal/md-uring-spin-c4 | not yet measured | not yet measured | 3056969 | not yet measured | not yet measured | 0 |

## f2-irqlocal/md-uring-spin-c64: lab, 60 s (first 2 s dropped), 64 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f2-irqlocal/md-uring-spin-c64 | not yet measured | not yet measured | 3057611 | not yet measured | not yet measured | 0 |

## f2-irqcore/md-epoll-spin-c1: lab, 60 s (first 2 s dropped), 1 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f2-irqcore/md-epoll-spin-c1 | not yet measured | not yet measured | 3056397 | not yet measured | not yet measured | 0 |

## f2-irqcore/md-epoll-spin-c16: lab, 60 s (first 2 s dropped), 16 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f2-irqcore/md-epoll-spin-c16 | not yet measured | not yet measured | 3056388 | not yet measured | not yet measured | 0 |

## f2-irqcore/md-epoll-spin-c256: lab, 60 s (first 2 s dropped), 256 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f2-irqcore/md-epoll-spin-c256 | not yet measured | not yet measured | 3056806 | not yet measured | not yet measured | 0 |

## f2-irqcore/md-epoll-spin-c4: lab, 60 s (first 2 s dropped), 4 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f2-irqcore/md-epoll-spin-c4 | not yet measured | not yet measured | 3056401 | not yet measured | not yet measured | 0 |

## f2-irqcore/md-epoll-spin-c64: lab, 60 s (first 2 s dropped), 64 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f2-irqcore/md-epoll-spin-c64 | not yet measured | not yet measured | 3056494 | not yet measured | not yet measured | 0 |

## f2-irqcore/md-mio-c1: lab, 60 s (first 2 s dropped),  connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f2-irqcore/md-mio-c1 | not yet measured | not yet measured | 3056477 | not yet measured | not yet measured | 0 |

## f2-irqcore/md-mio-c16: lab, 60 s (first 2 s dropped),  connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f2-irqcore/md-mio-c16 | not yet measured | not yet measured | 3056401 | not yet measured | not yet measured | 0 |

## f2-irqcore/md-mio-c256: lab, 60 s (first 2 s dropped),  connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f2-irqcore/md-mio-c256 | not yet measured | not yet measured | 3056602 | not yet measured | not yet measured | 0 |

## f2-irqcore/md-mio-c4: lab, 60 s (first 2 s dropped),  connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f2-irqcore/md-mio-c4 | not yet measured | not yet measured | 3056479 | not yet measured | not yet measured | 0 |

## f2-irqcore/md-mio-c64: lab, 60 s (first 2 s dropped),  connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f2-irqcore/md-mio-c64 | not yet measured | not yet measured | 3056479 | not yet measured | not yet measured | 0 |

## f2-irqcore/md-recv-busy-c1: lab, 60 s (first 2 s dropped), 1 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f2-irqcore/md-recv-busy-c1 | not yet measured | not yet measured | 3056390 | not yet measured | not yet measured | 0 |

## f2-irqcore/md-recv-busy-c16: lab, 60 s (first 2 s dropped), 16 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f2-irqcore/md-recv-busy-c16 | not yet measured | not yet measured | 3056470 | not yet measured | not yet measured | 0 |

## f2-irqcore/md-recv-busy-c256: lab, 60 s (first 2 s dropped), 256 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f2-irqcore/md-recv-busy-c256 | not yet measured | not yet measured | 3057204 | not yet measured | not yet measured | 0 |

## f2-irqcore/md-recv-busy-c4: lab, 60 s (first 2 s dropped), 4 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f2-irqcore/md-recv-busy-c4 | not yet measured | not yet measured | 3056403 | not yet measured | not yet measured | 0 |

## f2-irqcore/md-recv-busy-c64: lab, 60 s (first 2 s dropped), 64 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f2-irqcore/md-recv-busy-c64 | not yet measured | not yet measured | 3056401 | not yet measured | not yet measured | 0 |

## f2-irqcore/md-recv-busy-pq-c1: lab, 60 s (first 2 s dropped), 1 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f2-irqcore/md-recv-busy-pq-c1 | not yet measured | not yet measured | 3056396 | not yet measured | not yet measured | 0 |

## f2-irqcore/md-recv-busy-pq-c16: lab, 60 s (first 2 s dropped), 16 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f2-irqcore/md-recv-busy-pq-c16 | not yet measured | not yet measured | 3056468 | not yet measured | not yet measured | 0 |

## f2-irqcore/md-recv-busy-pq-c256: lab, 60 s (first 2 s dropped), 256 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f2-irqcore/md-recv-busy-pq-c256 | not yet measured | not yet measured | 3056691 | not yet measured | not yet measured | 0 |

## f2-irqcore/md-recv-busy-pq-c4: lab, 60 s (first 2 s dropped), 4 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f2-irqcore/md-recv-busy-pq-c4 | not yet measured | not yet measured | 3056457 | not yet measured | not yet measured | 0 |

## f2-irqcore/md-recv-busy-pq-c64: lab, 60 s (first 2 s dropped), 64 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f2-irqcore/md-recv-busy-pq-c64 | not yet measured | not yet measured | 3056401 | not yet measured | not yet measured | 0 |

## f2-irqcore/md-uring-spin-c1: lab, 60 s (first 2 s dropped), 1 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f2-irqcore/md-uring-spin-c1 | not yet measured | not yet measured | 3056401 | not yet measured | not yet measured | 0 |

## f2-irqcore/md-uring-spin-c16: lab, 60 s (first 2 s dropped), 16 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f2-irqcore/md-uring-spin-c16 | not yet measured | not yet measured | 3056399 | not yet measured | not yet measured | 0 |

## f2-irqcore/md-uring-spin-c256: lab, 60 s (first 2 s dropped), 256 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f2-irqcore/md-uring-spin-c256 | not yet measured | not yet measured | 3056711 | not yet measured | not yet measured | 0 |

## f2-irqcore/md-uring-spin-c4: lab, 60 s (first 2 s dropped), 4 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f2-irqcore/md-uring-spin-c4 | not yet measured | not yet measured | 3056401 | not yet measured | not yet measured | 0 |

## f2-irqcore/md-uring-spin-c64: lab, 60 s (first 2 s dropped), 64 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f2-irqcore/md-uring-spin-c64 | not yet measured | not yet measured | 3056388 | not yet measured | not yet measured | 0 |

## f2-irqcore/md-uring-sqpoll-c1: lab, 60 s (first 2 s dropped), 1 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f2-irqcore/md-uring-sqpoll-c1 | not yet measured | not yet measured | 3056494 | not yet measured | not yet measured | 0 |

## f2-irqcore/md-uring-sqpoll-c16: lab, 60 s (first 2 s dropped), 16 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f2-irqcore/md-uring-sqpoll-c16 | not yet measured | not yet measured | 3056394 | not yet measured | not yet measured | 0 |

## f2-irqcore/md-uring-sqpoll-c256: lab, 60 s (first 2 s dropped), 256 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f2-irqcore/md-uring-sqpoll-c256 | not yet measured | not yet measured | 3056502 | not yet measured | not yet measured | 0 |

## f2-irqcore/md-uring-sqpoll-c4: lab, 60 s (first 2 s dropped), 4 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f2-irqcore/md-uring-sqpoll-c4 | not yet measured | not yet measured | 3056474 | not yet measured | not yet measured | 0 |

## f2-irqcore/md-uring-sqpoll-c64: lab, 60 s (first 2 s dropped), 64 connection(s) per variant, kernel 7.0.0-1014-aws

| run | scheduled | issued | completed | late | rejected | censored |
|---|---|---|---|---|---|---|
| f2-irqcore/md-uring-sqpoll-c64 | not yet measured | not yet measured | 3056399 | not yet measured | not yet measured | 0 |

