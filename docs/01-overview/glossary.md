# Glossary

Key terms used throughout the SuperSip wiki.

| Term | Definition | See Also |
|------|-----------|----------|
| **3PCC** | Third-Party Call Control -- an external system sets up a call between two parties that did not initiate it. | [Concepts](../03-concepts/) |
| **ACD** | Automatic Call Distribution -- routes incoming calls to available agents based on rules and priority. | [Queue subsystem](../04-subsystems/) |
| **ACME** | Automatic Certificate Management Environment -- protocol for obtaining TLS certificates without manual steps. | [Operations](../04-subsystems/) |
| **B2BUA** | Back-to-Back User Agent -- a SIP entity that terminates one call leg and originates another, giving full control over both sides. | [Concepts](../03-concepts/) |
| **Bearer Token** | An opaque credential sent in the HTTP `Authorization` header to authenticate API requests. | [API reference](../04-subsystems/) |
| **BYE** | The SIP method that terminates an established call session. | [Concepts](../03-concepts/) |
| **CDR** | Call Detail Record -- a structured log of a completed call including participants, duration, and disposition. | [CDR subsystem](../04-subsystems/) |
| **Codec** | An algorithm that encodes and decodes audio (e.g., G.711, Opus) for transmission over the network. | [Concepts](../03-concepts/) |
| **CPaaS** | Communications Platform as a Service -- a cloud API that lets developers embed voice, messaging, and video into applications. | [Editions](editions.md) |
| **DID** | Direct Inward Dialing -- a phone number that routes directly to a specific endpoint or service inside the PBX. | [DID subsystem](../04-subsystems/) |
| **Dialplan** | A set of rules that determines how an incoming or outgoing call is routed based on the dialed number and context. | [Routing subsystem](../04-subsystems/) |
| **DTMF** | Dual-Tone Multi-Frequency -- the tones generated when pressing phone keypad buttons, used for IVR navigation. | [Concepts](../03-concepts/) |
| **Gateway** | A network element that bridges calls between different networks (e.g., SIP to PSTN). | [Gateway subsystem](../04-subsystems/) |
| **ICE** | Interactive Connectivity Establishment -- a framework for NAT traversal that finds the best path between two endpoints. | [Concepts](../03-concepts/) |
| **INVITE** | The SIP method that initiates a new call session. | [Concepts](../03-concepts/) |
| **IVR** | Interactive Voice Response -- a system that plays prompts and collects caller input via DTMF or speech. | [IVR subsystem](../04-subsystems/) |
| **Manipulation** | A conditional SIP header rewrite rule applied after routing selects a trunk. | [Routing subsystem](../04-subsystems/) |
| **NAT** | Network Address Translation -- a technique that maps private IP addresses to public ones, complicating direct media paths. | [Concepts](../03-concepts/) |
| **Queue** | A holding area where callers wait until an agent becomes available, with hold music and priority scheduling. | [Queue subsystem](../04-subsystems/) |
| **RBAC** | Role-Based Access Control -- a permission model where users are assigned roles that grant specific capabilities. | [Console subsystem](../04-subsystems/) |
| **REFER** | The SIP method used to transfer a call to another party. | [Concepts](../03-concepts/) |
| **REGISTER** | The SIP method by which a user agent tells the registrar its current contact address. | [Concepts](../03-concepts/) |
| **RTP** | Real-time Transport Protocol -- carries the actual audio (and video) media between call participants. | [Concepts](../03-concepts/) |
| **RWI** | Real-time WebSocket Interface -- SuperSip's JSON-over-WebSocket protocol for live call control. | [RWI subsystem](../04-subsystems/) |
| **SDP** | Session Description Protocol -- describes media capabilities (codecs, addresses, ports) exchanged during call setup. | [Concepts](../03-concepts/) |
| **SipFlow** | SuperSip's unified SIP+RTP capture format with hourly file rotation and on-demand playback. | [Recording subsystem](../04-subsystems/) |
| **SRTP** | Secure RTP -- encrypts the media stream to prevent eavesdropping. | [Concepts](../03-concepts/) |
| **STUN** | Session Traversal Utilities for NAT -- a protocol that discovers the public address of an endpoint behind NAT. | [Concepts](../03-concepts/) |
| **Translation** | A number rewrite rule applied before routing so the routing engine sees normalized numbers. | [Routing subsystem](../04-subsystems/) |
| **Trunk** | A SIP connection to an external carrier or service provider used for inbound and outbound PSTN calls. | [Trunk subsystem](../04-subsystems/) |
| **TURN** | Traversal Using Relays around NAT -- a relay server used when direct peer-to-peer media is impossible. | [Concepts](../03-concepts/) |
| **WebRTC** | Web Real-Time Communication -- a browser-native standard for voice and video that uses ICE, DTLS-SRTP, and SCTP. | [Concepts](../03-concepts/) |
| **Webhook** | An HTTP callback that SuperSip sends to your server when an event occurs (e.g., call ended, CDR ready). | [Webhook subsystem](../04-subsystems/) |

---
**Status:** Shipped
**Source:** Derived from README.md, PROJECT.md, and CARRIER-API.md terminology
**Last reviewed:** 2026-04-16
