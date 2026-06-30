# syntax=docker/dockerfile:1.7

# Stage 1 — chef: install cargo-chef on top of the rust toolchain image.
FROM rust:1.95-bookworm AS chef
RUN cargo install cargo-chef --locked
WORKDIR /build

# Stage 2 — planner: compute the dependency-only recipe from Cargo manifests.
# Only Cargo.toml / Cargo.lock changes invalidate this layer, so source-only
# edits skip straight to the cached `cook` layer below.
FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# Stage 3 — cook: build *only* the dependency graph using the recipe.
# This layer is reused as long as the recipe hash is unchanged.
FROM chef AS cook
COPY --from=planner /build/recipe.json recipe.json
RUN cargo chef cook --release --no-default-features \
    --features fuse,vendored-openssl --recipe-path recipe.json

# Stage 4 — build the actual binaries; deps come from the cooked cache.
FROM cook AS builder
COPY . .
RUN cargo build --release --no-default-features --features fuse,vendored-openssl \
    --bin hf-mount-fuse --bin hf-mount-fuse-sidecar

# Runtime
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends libfuse3-3 ca-certificates && rm -rf /var/lib/apt/lists/*

# TRUST INTERNAL ACCELERATOR S3
COPY <<EOF /usr/local/share/ca-certificates/harica-geant.crt
-----BEGIN CERTIFICATE-----
MIIDtjCCAp4CCQDFm01lBlHbcDANBgkqhkiG9w0BAQsFADCBnDELMAkGA1UEBhMC
QVQxEDAOBgNVBAgMB0F1c3RyaWExEjAQBgNVBAcMCUxheGVuYnVyZzEOMAwGA1UE
CgwFSUlBU0ExDDAKBgNVBAsMA0lDVDEgMB4GA1UEAwwXY2VydGlmaWNhdGUuaWlh
c2EuYWMuYXQxJzAlBgkqhkiG9w0BCQEWGGljdC5oZWxwZGVza0BpaWFzYS5hYy5h
dDAeFw0yMzAzMjMxMzA3MDVaFw00MzEwMDQxMzA3MDVaMIGcMQswCQYDVQQGEwJB
VDEQMA4GA1UECAwHQXVzdHJpYTESMBAGA1UEBwwJTGF4ZW5idXJnMQ4wDAYDVQQK
DAVJSUFTQTEMMAoGA1UECwwDSUNUMSAwHgYDVQQDDBdjZXJ0aWZpY2F0ZS5paWFz
YS5hYy5hdDEnMCUGCSqGSIb3DQEJARYYaWN0LmhlbHBkZXNrQGlpYXNhLmFjLmF0
MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEA08pA8TlPOhQ1rg2zBXgy
2ZOAPSB1GKsxuLhgqRh9MxBkfBKqqwbuvt2r/DFrqOccKY2njgKdwmxweqcp2T/H
hH756LOHiEZNvv6zBodpkYMF+VxSkepVTPIvNdHCFvy12c2uM4dL7pHhOqVBf6Ly
2wfmP/fj0mwJeRLx8wDvyMUkKf3kC6UTvT5AbK0LI6jeyLxJlzF6YQqGK6L52RS1
Pbnu4gIODHJHsNshg1QmBCQYI6v1L4FXgosNbksPf05wL2SB+DI/kktLP8qSXtkx
IV7WBPsilnu8R0md2wHL+WUNTwmukB2W6KlRqoqSgZJ3nNRaqnOq8HLU3FR0Fjn9
JwIDAQABMA0GCSqGSIb3DQEBCwUAA4IBAQAexeWy9rEH3x0SLK2D8VBgggIJv3iY
ZPeMMAotF9fop/+Tf4KrTs3tbs4mwDmg9dlxMNlAYvdOyC1mfSfg5qjCF71WRxY2
a+9sIb2rvmaQ5pEuO8i7RGgTOeHj5E7f8UoCwRnC+JUw52eOTjcCfw1QxoWGieiB
whrNbNhjI0xWDNxLb2VZ0rfFtO6lEFzVQbF6GIXq4QOjxtWRV/DQKX+S4aZgmniT
0vTP1bVoS1vHkidVAFZ9v82pCGZFXpjku/gjjmO4Yc/In/WeqiyKZ2HRzIO/ZcGk
nP6/j/YnBT9ayxJE5ku2OXNh/EiuNZRytdImcik6K4TePQjhvP4gXmK5
-----END CERTIFICATE-----
EOF

COPY <<EOF /usr/local/share/ca-certificates/backend.crt
-----BEGIN CERTIFICATE-----
MIIDMzCCAhugAwIBAgIUSHo/C2CC958yC+vf6YsCq7m8x4gwDQYJKoZIhvcNAQEL
BQAwEjEQMA4GA1UEAwwHbG9jYWxpcDAeFw0yNjA2MzAyMDU2MTdaFw0zNjA2Mjcy
MDU2MTdaMBIxEDAOBgNVBAMMB2xvY2FsaXAwggEiMA0GCSqGSIb3DQEBAQUAA4IB
DwAwggEKAoIBAQDXM6REjy/7gj9MGWYWsX3bRWM8nxwdIBXMmZL7fiG9A7CleBdZ
w8hvq21YEfgkobrB1rWvmWjc+Uwo8p6O3WE27N+g7emwZOCHWx3Hz2pnout4NE+r
YGLrey4HVOOa2jKg/2Kg5aHfNuhv3HMDAAPvRKOuQbAfkeOiSMMpZ9P2wbPLTdqh
3FH9H7WxLMCQx6Fc3yhGV70NS2vrbiziRncXmwSMt9ShwPLGqpePZkRqSsgu2fVe
GQUC7bbYYah+qy2S+sgZBC1usdSJ8qq5Dhom/3w2v/rCX4KbNxqIZUeD3rsl2vTN
tqrNgeLXb0W1TFOwnFfPIuwCQhzQ9r6KbRi/AgMBAAGjgYAwfjAdBgNVHQ4EFgQU
Bpiu1uOu6EuH5g4CA8S9Y6E0N+gwHwYDVR0jBBgwFoAUBpiu1uOu6EuH5g4CA8S9
Y6E0N+gwDwYDVR0TAQH/BAUwAwEB/zArBgNVHREEJDAiggdsb2NhbGlwggZ3ZWJf
YmWCCWxvY2FsaG9zdIcEfwAAATANBgkqhkiG9w0BAQsFAAOCAQEAw6RDq0yJ5LOK
Pcr8eMSk34V9heJBm3iaYeuvG4KGkJVTi5/Im0WdPV0/64bLVBZIjDySVOFkrzyu
tIeAfURzF+UPFP6yZ94gIkvgscKY4LRpjPRtctkzijHhmNJGB5YOmnRM+U+0ODzO
og+s5tcslXQnU4xytnj0/g9Hz1PT5Q0cRDmlFiEoLWEXUg+XMH4forjymuDd7cIV
eboBZMJCt1Yns08MpzRXFtWxh9TKpvnyVfNhM+W92uA70lzT6mcukSBIBR/ZHjpE
SpcX9Wn8wKDj5l7u6ueGdd+e6+5JPIBRLyPIQVnIFUovt8vo9/lOc1Rs67uzksjI
2dPlB+br4A==
-----END CERTIFICATE-----
EOF

COPY <<EOF /usr/local/share/ca-certificates/minio.crt
-----BEGIN CERTIFICATE-----
MIIEzTCCA7WgAwIBAgIUe88S98iszU8TxVQL7fQhi48wKXgwDQYJKoZIhvcNAQEL
BQAwfjELMAkGA1UEBhMCQVUxDTALBgNVBAgMBFdpZW4xDTALBgNVBAcMBFdpZW4x
EDAOBgNVBAoMB0JhYmJsZXIxCzAJBgNVBAsMAklUMRAwDgYDVQQDDAdsb2NhbGlw
MSAwHgYJKoZIhvcNAQkBFhF3cnVmZXNoQGdtYWlsLmNvbTAeFw0yNjA1MTkyMDEz
MjRaFw0zNjA1MTYyMDEzMjRaMH4xCzAJBgNVBAYTAkFVMQ0wCwYDVQQIDARXaWVu
MQ0wCwYDVQQHDARXaWVuMRAwDgYDVQQKDAdCYWJibGVyMQswCQYDVQQLDAJJVDEQ
MA4GA1UEAwwHbG9jYWxpcDEgMB4GCSqGSIb3DQEJARYRd3J1ZmVzaEBnbWFpbC5j
b20wggEiMA0GCSqGSIb3DQEBAQUAA4IBDwAwggEKAoIBAQCgzrLzoqIy2nIV5C5n
RJzxETJQGEqtAJIoQiHVgxTMJ83RT2FhKO15ClIApGt5973qvxambKquAkf5e9qj
f7Iit64UmEQzSPCLcTwLJukJD6jpNcHbSTtwySI9Ox+1kYzeuYitbWmg54DiIwIJ
3WApuI8/5yIwc9OQmUYX8aFxTIJG3DxmRosecwW7ha/vquPpRvwKbxh+Thr5irds
ELrNHRM8bB6O9HQD9KZADxPoH2ERaYjWBICjUr1UQqNzFsY8Cqk1uXSWIJ6cEhm8
MvodZV/CKs3sJwsUaGMsLLUS4TZ4ubVEnfEKKm/pVEwVcAqtiDed/NcPKXuEZMaj
yZVBAgMBAAGjggFBMIIBPTAdBgNVHQ4EFgQUaB+ScvBCTu8bDoRuF6J4ow1QaHow
HwYDVR0jBBgwFoAUaB+ScvBCTu8bDoRuF6J4ow1QaHowCQYDVR0TBAIwADALBgNV
HQ8EBAMCBaAwHQYDVR0lBBYwFAYIKwYBBQUHAwEGCCsGAQUFBwMCMIHDBgNVHREE
gbswgbiCCWxvY2FsaG9zdIIHbG9jYWxpcIIFbWluaW+CCHJlZ2lzdHJ5ggZ3ZWJf
YmWCHmx1bWV4cC1sb2NhbC5hY2Ntcy5paWFzYS5hYy5hdIIcbHVtZXhwLWxvYy5h
Y2Ntcy5paWFzYS5hYy5hdIIPbHBkLmlpYXNhLmFjLmF0gg9hZG1pbi5sb2NhbGhv
c3SCEXRlbmFudDEubG9jYWxob3N0ggpsb2NhbHJlY29uhwR/AAABhwQKAACNMA0G
CSqGSIb3DQEBCwUAA4IBAQCCc9RQ/RFKO/mHhJNcPmEwBJVOl1l7rH08kkMvJ+8d
6BTVv63EV+94R/scuOx78tUcbEweUmApIxpc/pRSGjY7vN9WILGEmTJtnOS1gPpO
+3ieZYLtJ12osV34OEoS9o8XTHLNALtdCY+YOkgjQY5dCbQSySHrEKXJeo331QWu
e3jvI/PjBtmuwnK3Bhwxb9WwVdIf5DBDx8V6BkHytykLZVr4UHIRB7y7fkAqcHTu
u4Osx4cDLsp7QXTyq9inzSBVGlSDo/DfocO0Om8pvRD2E/jjz5L6dRdHf6/+tpZG
NUYknrHgkoZ0ibwV2J9H5hBmfeWvtzEJQvlEghxpmbo1
-----END CERTIFICATE-----
EOF

RUN update-ca-certificates && \
    cat /usr/local/share/ca-certificates/harica-geant.crt \
    /usr/local/share/ca-certificates/backend.crt \
    /usr/local/share/ca-certificates/minio.crt \
    >> /etc/ssl/certs/ca-certificates.crt

ENV SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt
ENV SSL_CERT_DIR=/etc/ssl/certs
# END TRUST INTERNAL ACCELERATOR S3

COPY --from=builder /build/target/release/hf-mount-fuse /usr/local/bin/
COPY --from=builder /build/target/release/hf-mount-fuse-sidecar /usr/local/bin/
ENTRYPOINT ["/usr/local/bin/hf-mount-fuse"]
