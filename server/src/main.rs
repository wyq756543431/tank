extern crate bincode;
extern crate env_logger;
#[macro_use]
extern crate log;
extern crate tank;
extern crate websocket;
use bincode::{deserialize, serialize};

use tank::utils::duration_to_milis;
use tank::{SyncData, KeyEvent, GAME, MSG_DISCONNECT, MSG_KEY_EVENT, MSG_CONNECT, MSG_START, MSG_SYNC_DATA, SERVER_IP, SERVER_MSG_PLAYERS, SERVER_MSG_EVENT, SERVER_MSG_ERR,
           SERVER_MSG_SYNC, Player, SERVER_MSG_IP, SERVER_SYNC_DELAY};

use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};
use std::thread;
use websocket::OwnedMessage;
use websocket::sync::Server;
use std::sync::{Arc, RwLock};
use std::collections::HashMap;
use websocket::result::WebSocketError;
use std::io::ErrorKind;

use env_logger::Builder;
use log::LevelFilter;

type Writer = websocket::sender::Writer<std::net::TcpStream>;

fn main() {
    let mut builder = Builder::from_default_env();

    builder
        .filter(None, LevelFilter::Info)
        .default_format_timestamp(true)
        .init();

    let (game_sender, game_receiver): (Sender<(String, Vec<u8>)>, Receiver<(String, Vec<u8>)>) =
        channel();
    let connections: Arc<RwLock<HashMap<String, Writer>>> = Arc::new(RwLock::new(HashMap::new()));

    //启动游戏线程
    let connections_clone = connections.clone();
    let builder = thread::Builder::new().name("tank_game".into());
    let _gs = builder.spawn(move || {
        GAME.with(|game| {
            //let mut total_frames = 0;
            let start_time = Instant::now();
            let mut last_time = start_time.elapsed();
            let mut game = game.borrow_mut();
            //下一次同步(广播)数据的时间
            let mut next_sync_time = start_time.elapsed();
            loop {
                let timestamp = start_time.elapsed();
                let elapsed_ms = timestamp - last_time;
                //let now = Instant::now();
                //处理websocket传来的消息
                let mut messages = vec![];
                if let Ok((ip, mut msg)) = game_receiver.try_recv() {
                    if msg.len()>0 {
                        let msg_id = msg.remove(0);
                        match msg_id {
                            MSG_CONNECT => {
                                if let Ok(mut encoded) = serialize(&ip.to_string()) {
                                    encoded.insert(0, SERVER_MSG_IP);
                                    //send_binary_message(connections_clone.clone(), &ip, encoded);
                                    messages.push(encoded);
                                    info!("下发IP {:?}", ip);
                                }
                            }
                            MSG_START => {
                                //玩家加入游戏
                                let r: Result<String, _> = deserialize(&msg[..]);
                                if let Ok(username) = r {
                                    info!("join_game ip={} username={}", ip, username);
                                    game.server_join_game(ip.clone(), username);
                                    //下发玩家列表
                                    let players = game.players().iter().map(|(id, player)|{ (*id, player.name.clone()) }).collect::<Vec<(u32, String)>>();
                                    if let Ok(mut encoded) = serialize(&players) {
                                        encoded.insert(0, SERVER_MSG_PLAYERS);
                                        //send_binary_message(connections_clone.clone(), &ip, encoded);
                                        messages.push(encoded);
                                    }
                                } else {
                                    println!("MSG_START 消息解析失败 {:?}", r.err());
                                }
                            }

                            MSG_DISCONNECT => {
                                info!("玩家离开游戏{}", ip);
                                //玩家断开连接
                                game.server_leave_game(ip);
                            }

                            MSG_KEY_EVENT => {
                                //info!("MSG_KEY_EVENT");
                                //玩家上传按键事件
                                let r: Result<(KeyEvent, i32, u32), _> = deserialize(&msg[..]);
                                if let Ok((event, key, uid)) = r {
                                    game.server_on_key_event(event, key, uid);
                                } else {
                                    println!("MSG_KEY_EVENT 消息解析失败 {:?}", r.err());
                                }
                            }

                            MSG_SYNC_DATA => {
                                //玩家同步数据
                                let r: Result<SyncData, _> = deserialize(&msg[..]);
                                if let Ok(data) = r {
                                    //game.server_on_key_event(event, key, uid);
                                    game.server_update_player(ip, data);
                                } else {
                                    println!("MSG_KEY_EVENT 消息解析失败 {:?}", r.err());
                                }
                            }

                            other => info!("未定义消息: id={}", other),
                        }
                    }
                }
                game.server_update(duration_to_milis(&elapsed_ms));

                //同步数据
                if timestamp >= next_sync_time {
                    if game.players().len() > 0{
                        let sync_data = game.get_sync_data();
                        if let Ok(mut encoded) = serialize(&sync_data) {
                            encoded.insert(0, SERVER_MSG_SYNC);
                            //broad_cast_binary_message(connections_clone.clone(), encoded);
                            messages.push(encoded);
                        }
                    }
                    next_sync_time = timestamp + Duration::from_millis(SERVER_SYNC_DELAY);
                }

                //广播事件
                let events = game.get_server_events();
                if events.len()>0{
                    if let Ok(mut encoded) = serialize(&events) {
                        encoded.insert(0, SERVER_MSG_EVENT);
                        //broad_cast_binary_message(connections_clone.clone(), encoded);
                        messages.push(encoded);
                    }
                }
                if messages.len() > 0{
                    if let Ok(encoded) = serialize(&messages) {
                        broad_cast_binary_message(connections_clone.clone(), encoded);
                    }
                }

                last_time = timestamp;
                thread::park_timeout(Duration::from_millis(20));
                //total_frames += 1;
                // if total_frames%(50*60) == 0{
                //     info!("now={:?}", now.elapsed());
                // }
            }
        });
    });

    let server = Server::bind(SERVER_IP).unwrap();
    info!("服务器已启动 {}", SERVER_IP);
    for request in server.filter_map(Result::ok) {
        let game_sender_clone = game_sender.clone();
        let ws_connections = connections.clone();
        thread::spawn(move || {
            let client = request.accept().unwrap();
            let ip = client.peer_addr().unwrap();

            info!("创建连接: {}", ip);

            let (mut receiver, mut sender) = client.split().unwrap();
            if let Ok(mut map) = ws_connections.write() {
                map.insert(ip.to_string(), sender);
            }

            let connections_clone = ws_connections.clone();
            for message in receiver.incoming_messages() {
                //info!("on message:{:?}", message);
                if message.is_err() {
                    info!("消息错误: {:?}", message.err());
                    break;
                }
                let message = message.unwrap();

                match message {
                    OwnedMessage::Text(text) => {
                        info!("on text message:{}", text);
                    }
                    OwnedMessage::Binary(buffer) => {
                        //info!("binary {}", buffer.len());
                        //info!("{:?}", buffer);
                        let _ = game_sender_clone.send((ip.to_string(), buffer));
                    }
                    OwnedMessage::Close(_) => {
                        info!("Client {} Close断开连接", ip);
                        break;
                    }
                    other => {
                        info!("other message {:?}", other);
                    }
                }
            }
            connections_clone.write().unwrap().remove(&ip.to_string());
            let _ = game_sender_clone.send((ip.to_string(), vec![MSG_DISCONNECT]));
            info!("连接关闭: {}", ip);
        });
    }

    info!("游戏服务结束.");
}

fn send_message(connections: Arc<RwLock<HashMap<String, Writer>>>, uuid: &String, message: String) {
    //info!("send_message: {} to {}", message, uuid);
    let mut connections = connections.write().unwrap();
    if !connections.contains_key(uuid) {
        info!("uuid不存在 {}", uuid);
        return;
    }
    if let Err(err) = connections
        .get_mut(uuid)
        .unwrap()
        .send_message(&OwnedMessage::Text(message))
    {
        info!("消息发送失败: {:?}", err);
        match err {
            WebSocketError::IoError(err) => {
                if err.kind() == ErrorKind::ConnectionAborted {
                    connections.remove(uuid);
                }
            }
            _ => {}
        }
    }
}

fn broad_cast_message(connections: Arc<RwLock<HashMap<String, Writer>>>, message: String) {
    //info!("broad_cast_message: {}", message);
    let mut aborted_connections = vec![];
    let message = OwnedMessage::Text(message);
    for (addr, sender) in connections.write().unwrap().iter_mut() {
        if let Err(err) = sender.send_message(&message) {
            info!("消息发送失败: {:?}", err);
            match err {
                WebSocketError::IoError(err) => {
                    if err.kind() == ErrorKind::ConnectionAborted
                        || err.kind() == ErrorKind::BrokenPipe
                    {
                        aborted_connections.push(addr.clone());
                    }
                }
                _ => {}
            }
        }
    }
    let mut connections = connections.write().unwrap();
    for aborted_addr in aborted_connections {
        connections.remove(&aborted_addr);
    }
}

fn send_binary_message(
    connections: Arc<RwLock<HashMap<String, Writer>>>,
    uuid: &String,
    message: Vec<u8>,
) {
    //info!("send_message: {} to {}", message, uuid);
    let mut connections = connections.write().unwrap();
    if !connections.contains_key(uuid) {
        info!("uuid不存在 {}", uuid);
        return;
    }
    if let Err(err) = connections
        .get_mut(uuid)
        .unwrap()
        .send_message(&OwnedMessage::Binary(message))
    {
        info!("消息发送失败: {:?}", err);
        match err {
            WebSocketError::IoError(err) => {
                if err.kind() == ErrorKind::ConnectionAborted {
                    connections.remove(uuid);
                }
            }
            _ => {}
        }
    }
}

fn broad_cast_binary_message(connections: Arc<RwLock<HashMap<String, Writer>>>, message: Vec<u8>) {
    //info!("broad_cast_message: {}", message);
    let mut aborted_connections = vec![];
    let message = OwnedMessage::Binary(message);
    for (addr, sender) in connections.write().unwrap().iter_mut() {
        if let Err(err) = sender.send_message(&message) {
            info!("消息发送失败: {:?}", err);
            match err {
                WebSocketError::IoError(err) => {
                    if err.kind() == ErrorKind::ConnectionAborted
                        || err.kind() == ErrorKind::BrokenPipe
                    {
                        aborted_connections.push(addr.clone());
                    }
                }
                _ => {}
            }
        }
    }
    let mut connections = connections.write().unwrap();
    for aborted_addr in aborted_connections {
        connections.remove(&aborted_addr);
    }
}
