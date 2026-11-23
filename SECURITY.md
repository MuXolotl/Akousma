# Security policy

## Supported versions

| Version | Support |
| :------ | :------ |
| Latest 0.1.x | Supported |

## Reporting a vulnerability

Please report security issues privately, without opening a public issue:

1. Go to the repository's **Security** tab and choose **Report a vulnerability**
   (GitHub private security advisories), or
2. if that is not available, open a public issue asking for a private channel.
   Include no details beyond "I have a security report".

Please include the affected version, a short description of the impact, and a
minimal file or byte sequence that reproduces the problem if you can. A crash or
an out-of-memory condition on a corrupted file is in scope, however small the
file is.

You can expect an acknowledgement within a week. There is no bug bounty.

## Input handling

The library has no third-party dependencies and forbids `unsafe` code. Parsers
check input sizes against configured limits before reserving memory.
`FormatLimits::max_metadata_bytes` limits metadata input; `max_packet_bytes`
limits each audio packet or compressed block. These limits bound input sizes,
not the exact memory used by the decoded result.
Reports of memory exhaustion or unusually high memory use are welcome. Include
the input and configured limits when possible.

The library treats media files as data. It does not execute embedded content,
access audio devices, connect to the network or start threads.

## Not included

The library decodes audio but does not play it or apply gain or other audio
effects. Fuzzing is performed with separate development tools, not during normal
library use.
