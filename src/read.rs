use std::io::{BufReader, Read, Take, Cursor};
use std::net::TcpStream;
use std::sync::Arc;
use byteorder::{ReadBytesExt, BigEndian};
use {Error, Result, ConnectReturnCode, SubscribeTopic, SubscribeReturnCodes};
use {PacketType, Header, QoS, LastWill, Protocol, PacketIdentifier, MULTIPLIER};

use mqtt::{
    Packet,
    Connect,
    Connack,
    Publish,
    Subscribe,
    Suback,
    Unsubscribe
};

pub trait MqttRead: ReadBytesExt {
    fn deserialize(&mut self, remaining_len: usize, header: Header) -> Result<Packet> {
        if remaining_len == 0 {
            // no payload packets
            return match header.typ {
                PacketType::Pingreq => Ok(Packet::Pingreq),
                PacketType::Pingresp => Ok(Packet::Pingresp),
                PacketType::Disconnect => Ok(Packet::Disconnect),
                _ => Err(Error::PayloadRequired)
            };
        }

        let mut raw_packet = self.take(remaining_len as u64);

        match header.typ {
            PacketType::Connect => Ok(Packet::Connect(raw_packet.read_connect(header)?)),
            PacketType::Connack => Ok(Packet::Connack(raw_packet.read_connack(header)?)),
            PacketType::Publish => Ok(Packet::Publish(raw_packet.read_publish(header)?)),
            PacketType::Puback => {
                if remaining_len != 2 {
                    return Err(Error::PayloadSizeIncorrect)
                }
                let pkid = raw_packet.read_u16::<BigEndian>()?;
                Ok(Packet::Puback(PacketIdentifier(pkid)))
            },
            PacketType::Pubrec => {
                if remaining_len != 2 {
                    return Err(Error::PayloadSizeIncorrect)
                }
                let pkid = raw_packet.read_u16::<BigEndian>()?;
                Ok(Packet::Pubrec(PacketIdentifier(pkid)))
            },
            PacketType::Pubrel => {
                if remaining_len != 2 {
                    return Err(Error::PayloadSizeIncorrect)
                }
                let pkid = raw_packet.read_u16::<BigEndian>()?;
                Ok(Packet::Pubrel(PacketIdentifier(pkid)))
            },
            PacketType::Pubcomp => {
                if remaining_len != 2 {
                    return Err(Error::PayloadSizeIncorrect)
                }
                let pkid = raw_packet.read_u16::<BigEndian>()?;
                Ok(Packet::Pubcomp(PacketIdentifier(pkid)))
            },
            PacketType::Subscribe => Ok(Packet::Subscribe(raw_packet.read_subscribe(header)?)),
            PacketType::Suback => Ok(Packet::Suback(raw_packet.read_suback(header)?)),
            PacketType::Unsubscribe => Ok(Packet::Unsubscribe(raw_packet.read_unsubscribe(header)?)),
            PacketType::Unsuback => {
                if remaining_len != 2 {
                    return Err(Error::PayloadSizeIncorrect)
                }
                let pkid = raw_packet.read_u16::<BigEndian>()?;
                Ok(Packet::Unsuback(PacketIdentifier(pkid)))
            },
            PacketType::Pingreq => Err(Error::IncorrectPacketFormat),
            PacketType::Pingresp => Err(Error::IncorrectPacketFormat),
            _ => Err(Error::UnsupportedPacketType)
        }
    }


    fn read_packet(&mut self) -> Result<Packet> {
        let hd = self.read_u8()?;
        let remaining_len = self.read_remaining_length()?;

        let header = Header::new(hd, remaining_len)?;
        self.deserialize(remaining_len, header)
    }

    fn read_packet_with_len(&mut self) -> Result<(Packet, usize)> {
        let hd = self.read_u8()?;
        let remaining_len = self.read_remaining_length()?;

        let header = Header::new(hd, remaining_len)?;
        let header_len = header.len();

        Ok((self.deserialize(remaining_len, header)?, header_len + remaining_len))
    }

    fn read_connect(&mut self, _: Header) -> Result<Connect> {
        let protocol_name = self.read_mqtt_string()?;
        let protocol_level = self.read_u8()?;
        let protocol = Protocol::new(protocol_name.as_ref(), protocol_level)?;

        let connect_flags = self.read_u8()?;
        let keep_alive = self.read_u16::<BigEndian>()?;
        let client_id = self.read_mqtt_string()?;

        let last_will = match connect_flags & 0b100 {
            0 => {
                if (connect_flags & 0b00111000) != 0 {
                    return Err(Error::IncorrectPacketFormat)
                }
                None
            },
            _ => {
                let will_topic = self.read_mqtt_string()?;
                let will_message = self.read_mqtt_string()?;
                let will_qod = QoS::from_u8((connect_flags & 0b11000) >> 3)?;
                Some(LastWill {
                    topic: will_topic,
                    message: will_message,
                    qos: will_qod,
                    retain: (connect_flags & 0b00100000) != 0
                })
            }
        };

        let username = match connect_flags & 0b10000000 {
            0 => None,
            _ => Some(self.read_mqtt_string()?)
        };

        let password = match connect_flags & 0b01000000 {
            0 => None,
            _ => Some(self.read_mqtt_string()?)
        };

        Ok(Connect {
                protocol,
                keep_alive,
                client_id,
                clean_session: (connect_flags & 0b10) != 0,
                last_will,
                username,
                password
        }
        )
    }

    fn read_connack(&mut self, header: Header) -> Result<Connack> {
        if header.len != 2 {
            return Err(Error::PayloadSizeIncorrect)
        }
        let flags = self.read_u8()?;
        let return_code = self.read_u8()?;
        Ok(Connack {
            session_present: (flags & 0x01) == 1,
            code: ConnectReturnCode::from_u8(return_code)?
        })
    }

    fn read_publish(&mut self, header: Header) -> Result<Publish> {
        let topic_name = self.read_mqtt_string();
        // Packet identifier exists where QoS > 0
        let pkid = if header.qos().unwrap() != QoS::AtMostOnce {
            Some(PacketIdentifier(self.read_u16::<BigEndian>()?))
        } else {
            None
        };
        let mut payload = Vec::new();
        self.read_to_end(&mut payload)?;

        Ok(Publish {
                dup: header.dup(),
                qos: header.qos()?,
                retain: header.retain(),
                topic_name: topic_name?,
            pkid: pkid,
                payload: Arc::new(payload)
            }
        )
    }

    fn read_subscribe(&mut self, header: Header) -> Result<Subscribe> {
        let pkid = self.read_u16::<BigEndian>()?;
        let mut remaining_bytes = header.len - 2;
        let mut topics = Vec::with_capacity(1);

        while remaining_bytes > 0 {
            let topic_filter = self.read_mqtt_string()?;
            let requested_qod = self.read_u8()?;
            remaining_bytes -= topic_filter.len() + 3;
            topics.push(SubscribeTopic { topic_path: topic_filter, qos: QoS::from_u8(requested_qod)? });
        };

        Ok(Subscribe {
            pkid: PacketIdentifier(pkid),
            topics
        })
    }

    fn read_suback(&mut self, header: Header) -> Result<Suback> {
        let pkid = self.read_u16::<BigEndian>()?;
        let mut remaining_bytes = header.len - 2;
        let mut return_codes = Vec::with_capacity(remaining_bytes);

        while remaining_bytes > 0 {
            let return_code = self.read_u8()?;
            if return_code >> 7 == 1 {
                return_codes.push(SubscribeReturnCodes::Failure)
            } else {
                return_codes.push(SubscribeReturnCodes::Success(QoS::from_u8(return_code & 0x3)?));
            }
            remaining_bytes -= 1
        };

        Ok(Suback {
            pkid: PacketIdentifier(pkid),
            return_codes
        })
    }

    fn read_unsubscribe(&mut self, header: Header) -> Result<Unsubscribe> {
        let pkid = self.read_u16::<BigEndian>()?;
        let mut remaining_bytes = header.len - 2;
        let mut topics = Vec::with_capacity(1);

        while remaining_bytes > 0 {
            let topic_filter = self.read_mqtt_string()?;
            remaining_bytes -= topic_filter.len() + 2;
            topics.push(topic_filter);
        };

        Ok(Unsubscribe {
            pkid: PacketIdentifier(pkid),
            topics
        })
    }

    fn read_payload(&mut self, len: usize) -> Result<Box<Vec<u8>>> {
        let mut payload = Box::new(Vec::with_capacity(len));
        self.take(len as u64).read_to_end(&mut *payload)?;
        Ok(payload)
    }

    fn read_mqtt_string(&mut self) -> Result<String> {
        let len = self.read_u16::<BigEndian>()? as usize;
        let mut data = Vec::with_capacity(len);
        self.take(len as u64).read_to_end(&mut data)?;
        Ok(String::from_utf8(data)?)
    }

    fn read_remaining_length(&mut self) -> Result<usize> {
        let mut mult: usize = 1;
        let mut len: usize = 0;
        let mut done = false;


        while !done {
            let byte = self.read_u8()? as usize;
            len += (byte & 0x7F) * mult;
            mult *= 0x80;
            if mult > MULTIPLIER {
                return Err(Error::MalformedRemainingLength);
            }
            done = (byte & 0x80) == 0
        }

        Ok(len)
    }
}

impl MqttRead for TcpStream {}
impl MqttRead for Cursor<Vec<u8>> {}
impl<T: Read> MqttRead for Take<T> where T: Read {}
impl<T: Read> MqttRead for BufReader<T> {}
impl<'a> MqttRead for &'a[u8]{}

#[cfg(test)]
mod test {
    use std::io::Cursor;
    use std::sync::Arc;
    use super::MqttRead;
    use {Protocol, LastWill, QoS, PacketIdentifier, ConnectReturnCode, SubscribeTopic, SubscribeReturnCodes};
    use mqtt::{
        Packet,
        Connect,
        Connack,
        Publish,
        Subscribe,
        Suback,
        Unsubscribe
    };

    #[test]
    fn read_packet_connect_mqtt_protocol_test() {
        let mut stream = Cursor::new(vec![
            0x10, 39,
            0x00, 0x04, 'M' as u8, 'Q' as u8, 'T' as u8, 'T' as u8,
            0x04,
            0b11001110, // +username, +password, -will retain, will qos=1, +last_will, +clean_session
            0x00, 0x0a, // 10 sec
            0x00, 0x04, 't' as u8, 'e' as u8, 's' as u8, 't' as u8, // client_id
            0x00, 0x02, '/' as u8, 'a' as u8, // will topic = '/a'
            0x00, 0x07, 'o' as u8, 'f' as u8, 'f' as u8, 'l' as u8, 'i' as u8, 'n' as u8, 'e' as u8, // will msg = 'offline'
            0x00, 0x04, 'r' as u8, 'u' as u8, 's' as u8, 't' as u8, // username = 'rust'
            0x00, 0x02, 'm' as u8, 'q' as u8 // password = 'mq'
        ]);

        let packet = stream.read_packet().unwrap();

        assert_eq!(packet, Packet::Connect(Connect {
            protocol: Protocol::MQTT(4),
            keep_alive: 10,
            client_id: "test".to_owned(),
            clean_session: true,
            last_will: Some(LastWill {
                topic: "/a".to_owned(),
                message: "offline".to_owned(),
                retain: false,
                qos: QoS::AtLeastOnce
            }),
            username: Some("rust".to_owned()),
            password: Some("mq".to_owned())
        }));
    }

    #[test]
    fn read_packet_connect_mqisdp_protocol_test() {
        let mut stream = Cursor::new(vec![
            0x10, 18,
            0x00, 0x06, 'M' as u8, 'Q' as u8, 'I' as u8, 's' as u8, 'd' as u8, 'p' as u8,
            0x03,
            0b00000000, // -username, -password, -will retain, will qos=0, -last_will, -clean_session
            0x00, 0x3c, // 60 sec
            0x00, 0x04, 't' as u8, 'e' as u8, 's' as u8, 't' as u8 // client_id
        ]);

        let packet = stream.read_packet().unwrap();

        assert_eq!(packet, Packet::Connect(Connect {
            protocol: Protocol::MQIsdp(3),
            keep_alive: 60,
            client_id: "test".to_owned(),
            clean_session: false,
            last_will: None,
            username: None,
            password: None
        }));
    }

    #[test]
    fn read_packet_connack_test() {
        let mut stream = Cursor::new(vec![0b00100000, 0x02, 0x01, 0x00]);
        let packet = stream.read_packet().unwrap();

        assert_eq!(packet, Packet::Connack(Connack {
            session_present: true,
            code: ConnectReturnCode::Accepted
        }));
    }

    #[test]
    fn read_packet_publish_qos1_test() {
        let mut stream = Cursor::new(vec![
            0b00110010, 11,
            0x00, 0x03, 'a' as u8, '/' as u8, 'b' as u8, // topic name = 'a/b'
            0x00, 0x0a, // pkid = 10
            0xF1, 0xF2, 0xF3, 0xF4
        ]);

        let packet = stream.read_packet().unwrap();

        assert_eq!(packet, Packet::Publish(Publish {
            dup: false,
            qos: QoS::AtLeastOnce,
            retain: false,
            topic_name: "a/b".to_owned(),
            pkid: Some(PacketIdentifier(10)),
            payload: Arc::new(vec![0xF1, 0xF2, 0xF3, 0xF4])
        }));
    }

    #[test]
    fn read_packet_publish_qos0_test() {
        let mut stream = Cursor::new(vec![
            0b00110000, 7,
            0x00, 0x03, 'a' as u8, '/' as u8, 'b' as u8, // topic name = 'a/b'
            0x01, 0x02
        ]);

        let packet = stream.read_packet().unwrap();

        assert_eq!(packet, Packet::Publish(Publish {
            dup: false,
            qos: QoS::AtMostOnce,
            retain: false,
            topic_name: "a/b".to_owned(),
            pkid: None,
            payload: Arc::new(vec![0x01, 0x02])
        }));
    }

    #[test]
    fn read_packet_puback_test() {
        let mut stream = Cursor::new(vec![0b01000000, 0x02, 0x00, 0x0A]);
        let packet = stream.read_packet().unwrap();

        assert_eq!(packet, Packet::Puback(PacketIdentifier(10)));
    }

    #[test]
    fn read_packet_subscribe_test() {
        let mut stream = Cursor::new(vec![
            0b10000010, 20,
            0x01, 0x04, // pkid = 260
            0x00, 0x03, 'a' as u8, '/' as u8, '+' as u8, // topic filter = 'a/+'
            0x00, // qos = 0
            0x00, 0x01, '#' as u8, // topic filter = '#'
            0x01, // qos = 1
            0x00, 0x05, 'a' as u8, '/' as u8, 'b' as u8, '/' as u8, 'c' as u8, // topic filter = 'a/b/c'
            0x02 // qos = 2
        ]);

        let packet = stream.read_packet().unwrap();

        assert_eq!(packet, Packet::Subscribe(Subscribe {
            pkid: PacketIdentifier(260),
            topics: vec![
                SubscribeTopic { topic_path: "a/+".to_owned(), qos: QoS::AtMostOnce },
                SubscribeTopic { topic_path: "#".to_owned(), qos: QoS::AtLeastOnce },
                SubscribeTopic { topic_path: "a/b/c".to_owned(), qos: QoS::ExactlyOnce }
            ]
        }));
    }

    #[test]
    fn read_packet_unsubscribe_test() {
        let mut stream = Cursor::new(vec![
            0b10100010, 17,
            0x00, 0x0F, // pkid = 15
            0x00, 0x03, 'a' as u8, '/' as u8, '+' as u8, // topic filter = 'a/+'
            0x00, 0x01, '#' as u8, // topic filter = '#'
            0x00, 0x05, 'a' as u8, '/' as u8, 'b' as u8, '/' as u8, 'c' as u8, // topic filter = 'a/b/c'
        ]);

        let packet = stream.read_packet().unwrap();

        assert_eq!(packet, Packet::Unsubscribe(Unsubscribe {
            pkid: PacketIdentifier(15),
            topics: vec![
                "a/+".to_owned(),
                "#".to_owned(),
                "a/b/c".to_owned()
            ]
        }));
    }

    #[test]
    fn read_packet_suback_test() {
        let mut stream = Cursor::new(vec![
            0x90, 4,
            0x00, 0x0F, // pkid = 15
            0x01, 0x80
        ]);

        let packet = stream.read_packet().unwrap();

        assert_eq!(packet, Packet::Suback(Suback {
            pkid: PacketIdentifier(15),
            return_codes: vec![SubscribeReturnCodes::Success(QoS::AtLeastOnce), SubscribeReturnCodes::Failure]
        }));
    }
}
