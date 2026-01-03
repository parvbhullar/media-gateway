-- ============================================
-- Kamailio Multi-Tenant Gateway Configuration
-- Dispatcher & Permissions Tables
-- ============================================

-- Version table (required by Kamailio for schema version checking)
CREATE TABLE IF NOT EXISTS version (
    id INT(10) UNSIGNED AUTO_INCREMENT PRIMARY KEY,
    table_name VARCHAR(32) NOT NULL,
    table_version INT UNSIGNED NOT NULL DEFAULT 0,
    UNIQUE KEY table_name_idx (table_name)
) ENGINE=InnoDB;

-- Insert version info for all tables
INSERT INTO version (table_name, table_version) VALUES
('dispatcher', 4),
('address', 6),
('trusted', 6),
('dialog', 7),
('dialog_vars', 1),
('acc', 5),
('missed_calls', 4);

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

-- Default Gateway Group (setid=1) - VAPI as primary (UDP)
INSERT INTO dispatcher (setid, destination, flags, priority, attrs, description) VALUES
(1, 'sip:66e592d6-d690-45bb-8996-18215af542d0.sip.vapi.ai:5060', 0, 10, 'weight=100;socket=udp:103.146.242.234:5080', 'VAPI Gateway - Primary');

-- LiveKit Gateway Group (setid=2) - UDP
INSERT INTO dispatcher (setid, destination, flags, priority, attrs, description) VALUES
(2, 'sip:3i5bvr312d9.sip.livekit.cloud:5060', 0, 10, 'weight=50;socket=udp:103.146.242.234:5080', 'LiveKit Gateway - Primary'),
(2, 'sip:61xh9s3ubwq.sip.livekit.cloud:5060', 0, 10, 'weight=50;socket=udp:103.146.242.234:5080', 'DigiPanda LiveKit Gateway'),
(2, 'sip:4gv2kcqpg2d.sip.livekit.cloud:5060', 0, 10, 'weight=50;socket=udp:103.146.242.234:5080', 'Voxket LiveKit Gateway'),
(2, 'sip:15j2dl095m2.sip.livekit.cloud:5060', 0, 10, 'weight=50;socket=udp:103.146.242.234:5080', 'Fabriq LiveKit Gateway');

-- Backup Gateway Group (setid=3) - Unpod Cloud (UDP)
INSERT INTO dispatcher (setid, destination, flags, priority, attrs, description) VALUES
(3, 'sip:sip-up-tt.unpod.tv:5060', 0, 10, 'weight=50;socket=udp:103.146.242.234:5080', 'Unpod Cloud Gateway 1'),
(3, 'sip:sip.unpod.tel:5060', 0, 5, 'weight=50;socket=udp:103.146.242.234:5080', 'Unpod Cloud Gateway 2');

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
(1, '10.230.73.220', 32, 5060, 'Vodafone-INT-Phony'),
(1, '10.230.73.0', 24, 0, 'Private-Subnet');

-- Group 2: Public Gateways (Trusted Voice AI endpoints)
-- Note: These are DNS names, Kamailio will resolve them
-- For IP-based ACL, you may need to add resolved IPs or use domain matching
INSERT INTO address (grp, ip_addr, mask, port, tag) VALUES
(2, '0.0.0.0', 0, 0, 'Allow-All-Public-Gateways');

-- ============================================
-- Trusted Table (for permissions module)
-- ============================================

CREATE TABLE IF NOT EXISTS trusted (
    id INT(10) UNSIGNED AUTO_INCREMENT PRIMARY KEY,
    src_ip VARCHAR(50) NOT NULL,
    proto VARCHAR(4) NOT NULL,
    from_pattern VARCHAR(64) DEFAULT NULL,
    ruri_pattern VARCHAR(64) DEFAULT NULL,
    tag VARCHAR(64) DEFAULT NULL,
    priority INT(10) NOT NULL DEFAULT 0
) ENGINE=InnoDB;

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
-- Group 1: VAPI Gateway (default)
-- Group 2: LiveKit Gateways
-- Group 3: Unpod Cloud (backup)
INSERT INTO tenant_config (tenant_id, source_ip, gateway_group, max_cps, max_concurrent, priority) VALUES
('vapi', '10.230.73.220', 1, 50, 500, 10),
('livekit', NULL, 2, 50, 500, 10),
('default', NULL, 1, 10, 100, 1);

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
