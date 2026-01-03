FROM debian:bookworm-slim

# Install Kamailio with required modules
RUN apt-get update && apt-get install -y \
    gnupg \
    wget \
    curl \
    && wget -qO- https://deb.kamailio.org/kamailiodebkey.gpg | apt-key add - \
    && echo "deb http://deb.kamailio.org/kamailio57 bookworm main" > /etc/apt/sources.list.d/kamailio.list \
    && apt-get update && apt-get install -y \
    kamailio \
    kamailio-mysql-modules \
    kamailio-extra-modules \
    kamailio-tls-modules \
    kamailio-json-modules \
    kamailio-xml-modules \
    kamailio-utils-modules \
    kamailio-presence-modules \
    && apt-get clean \
    && rm -rf /var/lib/apt/lists/*

# Create directories
RUN mkdir -p /var/log/kamailio /run/kamailio

# Expose SIP ports
EXPOSE 5060/udp 5060/tcp 5080/udp 5080/tcp

# Default command
CMD ["kamailio", "-DD", "-E", "-f", "/etc/kamailio/kamailio.cfg"]
