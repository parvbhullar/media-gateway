-- ============================================
-- Kamailio Multi-Tenant Gateway Configuration
-- Dispatcher & Permissions Tables
-- ============================================

-- Dispatcher table for public gateways (LiveKit endpoints)
-- Group 1: Default gateways
-- Group 2: Premium/Priority gateways  
-- Group 3: Backup gateways

CREATE TABLE IF NOT EXISTS dispatcher (
    id INT(10) UNSIGNED AUTO_INCREMENT PRIMARY KEY,
    setid INT NOT NULL DEFAULT 0,
    destination VARCHAR(192) NOT NULL DEFAULT '',
    flags INT NOT NULL DEFAULT 0,
    priority INT NOT NULL DEFAULT 0,
    attrs VARCHAR(198) NOT NULL DEFAULT '',
    description VARCHAR(64) NOT NULL DEFAULT ''
) ENGINE=InnoDB;

-- Default Gateway Group (setid=1)
-- These are your primary LiveKit SIP endpoints
INSERT INTO dispatcher (setid, destination, flags, priority, attrs, description) VALUES
(1, 'sip:livekit-gw1.unpod.ai:5060', 0, 10, 'weight=50;socket=udp:203.0.113.50:5080', 'LiveKit Gateway 1 - Primary'),
(1, 'sip:livekit-gw2.unpod.ai:5060', 0, 10, 'weight=50;socket=udp:203.0.113.50:5080', 'LiveKit Gateway 2 - Primary');

-- Premium Gateway Group (setid=2)
-- Lower latency / dedicated instances for premium tenants
INSERT INTO dispatcher (setid, destination, flags, priority, attrs, description) VALUES
(2, 'sip:livekit-premium1.unpod.ai:5060', 0, 10, 'weight=100;socket=udp:203.0.113.50:5080', 'LiveKit Premium Gateway 1'),
(2, 'sip:livekit-premium2.unpod.ai:5060', 0, 5, 'weight=50;socket=udp:203.0.113.50:5080', 'LiveKit Premium Gateway 2');

-- Backup Gateway Group (setid=3)
-- Failover destinations
INSERT INTO dispatcher (setid, destination, flags, priority, attrs, description) VALUES
(3, 'sip:livekit-backup.unpod.ai:5060', 0, 1, 'weight=100;socket=udp:203.0.113.50:5080', 'LiveKit Backup Gateway');

-- ============================================
-- Address/Permissions Table
-- For ACL control of telecom carriers & gateways
-- ============================================

CREATE TABLE IF NOT EXISTS address (
    id INT(10) UNSIGNED AUTO_INCREMENT PRIMARY KEY,
    grp INT(11) UNSIGNED NOT NULL DEFAULT 1,
    ip_addr VARCHAR(50) NOT NULL,
    mask INT NOT NULL DEFAULT 32,
    port SMALLINT(5) UNSIGNED NOT NULL DEFAULT 0,
    tag VARCHAR(64) DEFAULT NULL
) ENGINE=InnoDB;

-- Group 1: Telecom Carriers (Private Network)
INSERT INTO address (grp, ip_addr, mask, port, tag) VALUES
(1, '10.0.1.10', 32, 5060, 'Carrier-Primary'),
(1, '10.0.1.20', 32, 5060, 'Carrier-Secondary'),
(1, '10.0.1.0', 24, 0, 'Carrier-Subnet'),
(1, '192.168.1.0', 24, 0, 'Internal-Network');

-- Group 2: Public Gateways (Trusted LiveKit endpoints)
INSERT INTO address (grp, ip_addr, mask, port, tag) VALUES
(2, '203.0.113.100', 32, 0, 'LiveKit-GW1'),
(2, '203.0.113.101', 32, 0, 'LiveKit-GW2'),
(2, '203.0.113.102', 32, 0, 'LiveKit-Premium'),
(2, '203.0.113.103', 32, 0, 'LiveKit-Backup');

-- ============================================
-- Tenant Mapping Table (Custom)
-- Maps source IPs or identifiers to tenant configs
-- ============================================

CREATE TABLE IF NOT EXISTS tenant_config (
    id INT(10) UNSIGNED AUTO_INCREMENT PRIMARY KEY,
    tenant_id VARCHAR(64) NOT NULL,
    source_ip VARCHAR(50) DEFAULT NULL,
    source_prefix VARCHAR(20) DEFAULT NULL,
    gateway_group INT NOT NULL DEFAULT 1,
    max_cps INT NOT NULL DEFAULT 10,
    max_concurrent INT NOT NULL DEFAULT 100,
    priority INT NOT NULL DEFAULT 5,
    enabled TINYINT(1) NOT NULL DEFAULT 1,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    UNIQUE KEY idx_tenant (tenant_id),
    INDEX idx_source_ip (source_ip)
) ENGINE=InnoDB;

-- Sample tenant configurations
INSERT INTO tenant_config (tenant_id, source_ip, gateway_group, max_cps, max_concurrent, priority) VALUES
('tenant_askiitians', '10.0.1.50', 1, 20, 200, 5),
('tenant_mahindra', '10.0.1.51', 2, 50, 500, 10),
('tenant_onecard', '10.0.1.52', 2, 30, 300, 8),
('tenant_lg', '10.0.1.53', 1, 25, 250, 6),
('tenant_default', NULL, 1, 10, 100, 1);

-- ============================================
-- CDR Table (Call Detail Records)
-- ============================================

CREATE TABLE IF NOT EXISTS acc (
    id INT(10) UNSIGNED AUTO_INCREMENT PRIMARY KEY,
    method VARCHAR(16) NOT NULL DEFAULT '',
    from_tag VARCHAR(64) NOT NULL DEFAULT '',
    to_tag VARCHAR(64) NOT NULL DEFAULT '',
    callid VARCHAR(255) NOT NULL DEFAULT '',
    sip_code VARCHAR(3) NOT NULL DEFAULT '',
    sip_reason VARCHAR(128) NOT NULL DEFAULT '',
    time DATETIME NOT NULL,
    src_user VARCHAR(64) NOT NULL DEFAULT '',
    src_domain VARCHAR(128) NOT NULL DEFAULT '',
    src_ip VARCHAR(64) NOT NULL DEFAULT '',
    dst_user VARCHAR(64) NOT NULL DEFAULT '',
    dst_domain VARCHAR(128) NOT NULL DEFAULT '',
    INDEX acc_callid (callid)
) ENGINE=InnoDB;

CREATE TABLE IF NOT EXISTS missed_calls (
    id INT(10) UNSIGNED AUTO_INCREMENT PRIMARY KEY,
    method VARCHAR(16) NOT NULL DEFAULT '',
    from_tag VARCHAR(64) NOT NULL DEFAULT '',
    to_tag VARCHAR(64) NOT NULL DEFAULT '',
    callid VARCHAR(255) NOT NULL DEFAULT '',
    sip_code VARCHAR(3) NOT NULL DEFAULT '',
    sip_reason VARCHAR(128) NOT NULL DEFAULT '',
    time DATETIME NOT NULL,
    src_user VARCHAR(64) NOT NULL DEFAULT '',
    src_domain VARCHAR(128) NOT NULL DEFAULT '',
    src_ip VARCHAR(64) NOT NULL DEFAULT '',
    dst_user VARCHAR(64) NOT NULL DEFAULT '',
    dst_domain VARCHAR(128) NOT NULL DEFAULT '',
    INDEX mc_callid (callid)
) ENGINE=InnoDB;

-- ============================================
-- Dialog Table (Active Call State)
-- ============================================

CREATE TABLE IF NOT EXISTS dialog (
    id INT(10) UNSIGNED AUTO_INCREMENT PRIMARY KEY,
    hash_entry INT(10) UNSIGNED NOT NULL,
    hash_id INT(10) UNSIGNED NOT NULL,
    callid VARCHAR(255) NOT NULL,
    from_uri VARCHAR(255) NOT NULL,
    from_tag VARCHAR(64) NOT NULL,
    to_uri VARCHAR(255) NOT NULL,
    to_tag VARCHAR(64) NOT NULL,
    caller_cseq VARCHAR(20) NOT NULL,
    callee_cseq VARCHAR(20) NOT NULL,
    caller_route_set VARCHAR(512) DEFAULT NULL,
    callee_route_set VARCHAR(512) DEFAULT NULL,
    caller_contact VARCHAR(255) NOT NULL,
    callee_contact VARCHAR(255) NOT NULL,
    caller_sock VARCHAR(64) NOT NULL,
    callee_sock VARCHAR(64) NOT NULL,
    state INT(10) UNSIGNED NOT NULL,
    start_time INT(10) UNSIGNED NOT NULL,
    timeout INT(10) UNSIGNED NOT NULL DEFAULT 0,
    sflags INT(10) UNSIGNED NOT NULL DEFAULT 0,
    iflags INT(10) UNSIGNED NOT NULL DEFAULT 0,
    toroute_name VARCHAR(32) DEFAULT NULL,
    req_uri VARCHAR(255) NOT NULL,
    xdata VARCHAR(512) DEFAULT NULL,
    UNIQUE KEY hash_idx (hash_entry, hash_id)
) ENGINE=InnoDB;

CREATE TABLE IF NOT EXISTS dialog_vars (
    id INT(10) UNSIGNED AUTO_INCREMENT PRIMARY KEY,
    hash_entry INT(10) UNSIGNED NOT NULL,
    hash_id INT(10) UNSIGNED NOT NULL,
    dialog_key VARCHAR(128) NOT NULL,
    dialog_value VARCHAR(512) NOT NULL,
    KEY hash_idx (hash_entry, hash_id)
) ENGINE=InnoDB;
