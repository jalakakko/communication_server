use std::{
   io::{BufRead, Write, Read},
   env, 
   net::{TcpStream, SocketAddr}, sync::{Arc, Mutex},
   sync::mpsc::{TryRecvError,}, str::FromStr
};
use tokio;
use bson::{Document};
use mongodb::{Client as MDBClient, options::{ClientOptions, ResolverConfig}, Collection,};
use mongodb::bson::doc;
use chrono::prelude::*;
use futures::{stream::TryStreamExt};
use bincode;
use serde::{Serialize, Deserialize};
use uuid::Uuid;
use crossbeam::channel;

const CHAT_MAX_SIZE: usize = 10;
const ADDR: &str = "127.0.0.1";

#[derive(Serialize, Deserialize, Debug, Clone)]
struct User {
   name: String,
   id: String,
   main_stream: SocketAddr,
   chat_stream: SocketAddr,
   updater_stream: SocketAddr
}

impl User {
   fn new(
         name: String, 
         id: String, 
         main_stream: SocketAddr,
         chat_stream: SocketAddr,
         updater_stream: SocketAddr) -> User {
      User { 
         name, 
         id,
         main_stream,
         chat_stream,
         updater_stream,
      }
   }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct Channel {
   id: String,
   channel_name: String,
   users: Option<Vec<User>>,
   chat_msgs: Option<Vec<Message>>,
}

impl Channel {
   fn new(
      id: String, 
      channel_name: String,
      users: Option<Vec<User>>, 
      chat_msgs: Option<Vec<Message>> ) -> Channel {
         Channel { id, channel_name, users: None, chat_msgs: None }
   }
}

#[derive(Debug)]
struct Connection {
   id: String,
   main_stream: Option<TcpStream>,
   chat_stream: Option<TcpStream>, 
   updater_stream: Option<TcpStream>
}

impl Connection {
   fn new(
      id: String,
      main_stream: Option<TcpStream>,
      chat_stream: Option<TcpStream>,
      updater_stream: Option<TcpStream>) -> Connection {
         Connection {  id,  main_stream, chat_stream, updater_stream }
   }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct Message {
   id: String,
   sender_id: String,
   sender: String,
   date: String,
   content: String,
}

#[tokio::main]
async fn main() {
   //Database
   let client_uri = env::var("MONGODB_URI")
      .expect("You must set the MONGODB_URI environment var!");
   let options =
      ClientOptions::parse_with_resolver_config(&client_uri, ResolverConfig::cloudflare()).await.unwrap();
   let client = MDBClient::with_options(options).unwrap();
   let channels_db = client.database("fakecord");
   let channels: Collection<Document> = channels_db.collection("channels"); 
   let mut cursor = channels.find(
      None,
      None
   ).await.unwrap();  

   //Inting channelpool
   let channelpool = Arc::new(Mutex::new(Vec::<Channel>::new()));
   let mut increment = 1;
   while let Some(channel) = cursor.try_next().await.unwrap() {
      let mut channel_name = || -> String {
         let mut name = String::from("Server ");
         name.push_str(increment.to_string().as_str());
         increment+=1;
         name
      };

      let c = Channel::new(
         channel.get_object_id("_id").unwrap().to_string(),
         channel_name(),
         None,
         None
      );
      channelpool.lock().unwrap().push(c);
   }

   let (tx, rx) = std::sync::mpsc::channel::<String>();
   let (tx_chnl, rx_chnl) = std::sync::mpsc::channel::<Channel>();
   //TODO change all to crossbeam channels
   let (tx_msgs, rx_msgs) = channel::unbounded();
   let channelpool2 = channelpool.clone();
   tokio::task::spawn(async move {
      loop {
         let received = rx.recv().unwrap();
         match received.as_str() {
            "UPDATECHAT" => {
               let channel = rx_chnl.recv().unwrap();
               let object_id = mongodb::bson::oid::ObjectId::from_str(&channel.id).unwrap();
               let chat = channels.find_one(
                  doc! {"_id" : object_id}, None)
                  .await.unwrap().unwrap();

               let db_chat_len = chat.get_array("chat_msgs").unwrap().len(); 
               let channel_chat_len = channel.chat_msgs.as_ref().unwrap().len();
               let len = db_chat_len - channel_chat_len;
               let mut vec = vec![];
               if len as i32 >= 0  && channel_chat_len < CHAT_MAX_SIZE { 
                  let (_, right) = chat.get_array("chat_msgs").unwrap()
                     .split_at(db_chat_len - len);
                  
                  for msg in right {
                     let message: Message = bson::from_bson(msg.clone()).unwrap();
                     vec.push(message);
                  }  
                  tx_msgs.send(vec).unwrap(); 
               } 
               else { 
                  let last = chat.get_array("chat_msgs").unwrap().last().unwrap();
                  let message: Message = bson::from_bson(last.clone()).unwrap();
                  vec.push(message);
                  tx_msgs.send(vec).unwrap();
               }  
            },
            "INTCHAT" => { 
               let channel = rx_chnl.recv().unwrap();
               let channel_id = channel.id.as_str();
               let object_id = mongodb::bson::oid::ObjectId::from_str(channel_id).unwrap(); 
               let chat = channels.find_one(
                  doc! {"_id" : object_id}, None)
                  .await.unwrap().unwrap();

               let mut chat_msgs: Vec<Message> = vec![];
               for chnl in channelpool2.lock().unwrap().iter_mut() {
                  if chnl.id.contains(channel_id) { 
                     for msg in chat.get_array("chat_msgs").unwrap() {
                        let message: Message = bson::from_bson(msg.clone()).unwrap();
                        chat_msgs.push(message);
                     } 
                     chnl.chat_msgs.insert(chat_msgs);
                     break;
                  }
               }
            },
            //New chat message
            _ => {
               let mut split = received.splitn(4, ' ');
               let (channel_id, user_id, name, msg) = (
                  split.next().unwrap(),
                  split.next().unwrap(),
                  split.next().unwrap(),
                  split.next().unwrap()
               );
            
               let msg_id = Uuid::new_v4().to_string();
               let now = Local::now().to_string();
               let timestamp = now.split('.').next().unwrap();
               let object_id = mongodb::bson::oid::ObjectId::from_str(channel_id).unwrap(); 
               channels.update_one(
                  doc! {"_id" : object_id},
                  doc!{ "$push" : { "chat_msgs" : {
                     
                     "id": msg_id, "sender_id": user_id, "date": timestamp, "sender": name, "content": msg
                  } } }, 
                  None,
                  ).await.unwrap();

               let chat = channels.find_one(
                  doc! {"_id" : object_id}, None).await.unwrap().unwrap();

                //Deletes the oldest message from the DB
                if chat.get_array("chat_msgs").unwrap().len() > CHAT_MAX_SIZE {
                  channels.update_one( 
                     doc! {"_id" : object_id},
                     doc! {"$pop" : { "chat_msgs" : -1}}, None).await.unwrap();
               }

               let mut chat_msgs: Vec<Message> = vec![];
               for chnl in channelpool2.lock().unwrap().iter_mut() {
                  if chnl.id.contains(channel_id) {
                     for msg in chat.get_array("chat_msgs").unwrap() {
                        let message: Message = bson::from_bson(msg.clone()).unwrap();
                        chat_msgs.push(message);
                     }
                     chnl.chat_msgs.insert(chat_msgs); 
                     break;
                  }
               }
            }
         } 
      }
   });
   
   //"Main thread"
   let main_listener = std::net::TcpListener::bind(format!("{}:8082", ADDR).as_str()).unwrap(); 
   let updater_listener = std::net::TcpListener::bind(format!("{}:8083", ADDR).as_str()).unwrap();  
   let connectionpool = Arc::new(Mutex::new(Vec::new())); 
   
   loop {
      let (main_stream, _) = main_listener.accept().unwrap();
      let (updater_stream, _) = updater_listener.accept().unwrap();
      println!("New connection from {}", main_stream.peer_addr().unwrap()); 
      
      let connectionpool = connectionpool.clone();  
      let channelpool = channelpool.clone(); 
      let tx = tx.clone(); 
      let tx_chnl = tx_chnl.clone(); 
      let rx_msgs = rx_msgs.clone();

      std::thread::spawn( move || loop {   
         let tx_chnl = tx_chnl.clone();  
         let tx = tx.clone();
         let signal = catch_signal(&main_stream);
         if !signal.is_empty() {
            println!(" from {:?}: received signal : {}", main_stream.peer_addr(), signal);
            match signal.as_str() {
               "CONNECT" => {      
                  let channelpool = channelpool.clone();
                  let connectionpool = connectionpool.clone();
                  let updater_stream = updater_stream.try_clone().expect("Can't clone stream");
                  connection(
                     &main_stream, 
                     updater_stream, 
                     channelpool, 
                     connectionpool
                  );
               },
               "ADDCHANNEL" => {
                  send_channel_info(&main_stream, &channelpool); 
               },
               "UPDATEUSERS" => {
                  update_channel_users(&main_stream, &channelpool, &connectionpool);
               },
               "DISCONNECT" => {
                  disconnect(&main_stream, &channelpool, &connectionpool)
               },
               "CHATMSG" => {
                  let mut reader = std::io::BufReader::new(main_stream.try_clone().unwrap());
                  let mut line = String::new();
                  reader.read_line(&mut line).unwrap();
                  line.pop();
                  tx.send(line).unwrap();

               },
               "INTCHAT" => {
                  tx.send(String::from("INTCHAT")).unwrap();
                  let mut reader = std::io::BufReader::new(main_stream.try_clone().unwrap());
                  let mut channel = vec![0; 2048];
                  reader.read(&mut channel).unwrap();
                  let deserialized: Channel = bincode::deserialize(&channel).unwrap();
                  tx_chnl.send(deserialized).unwrap();
               },
               "UPDATECHAT" => {
                  tx.send(String::from("UPDATECHAT")).unwrap();
                  let mut reader = std::io::BufReader::new(main_stream.try_clone().unwrap());
                  let mut writer = std::io::BufWriter::new(main_stream.try_clone().unwrap());
                  let mut channel = vec![0; 10000];
                  reader.read(&mut channel).unwrap();
                  let deserialized: Channel = bincode::deserialize(&channel).unwrap();
                  tx_chnl.send(deserialized).unwrap();

                  let new_msgs = rx_msgs.recv().unwrap();
                  let serialized = bincode::serialize(&new_msgs).unwrap();
                  writer.write(&serialized).unwrap();
               },
               _ => ()
            }
         } 
      });
   }  
}

fn connection(
   main_stream: &TcpStream,
   updater_stream: TcpStream,
   channelpool: Arc<Mutex<Vec<Channel>>>,
   connectionpool: Arc<Mutex<Vec<Connection>>>) { 

      let chat_listener = std::net::TcpListener::bind(format!("{}:8081", ADDR).as_str()).unwrap();
      let (chat_stream, _) = chat_listener.accept().unwrap();
      
      let channel = catch_signal(&main_stream); 
      let v:Vec<&str> = channel.split(' ').collect();
      let name = v[1].to_string();
      let id = v[2].to_string(); 
      let main_addr = main_stream.peer_addr().unwrap();
      let chat_addr = chat_stream.peer_addr().unwrap();
      let updater_addr = updater_stream.peer_addr().unwrap();
      let connection = Connection::new(id.clone(), 
         Some(main_stream.try_clone().unwrap()), 
         Some(chat_stream.try_clone().unwrap()), 
         Some(updater_stream.try_clone().unwrap())
      ); 

      //Check for duplicate user Id's 
      let mut b = true;
      if !connectionpool.lock().unwrap().is_empty() {
         for conn in connectionpool.lock().unwrap().iter_mut() { 
            if conn.id.contains(&id) {
               conn.chat_stream.insert(chat_stream.try_clone().unwrap());
               b = false;
               break;
            }  
         }
         if b { 
         connectionpool.lock().unwrap().push(connection);
         }//TODO doublecheck this..
      } else { 
         connectionpool.lock().unwrap().push(connection);
      }

      let user = User::new(name,
          id.clone(), 
          main_addr, 
          chat_addr, 
          updater_addr);

      let mut current_channel = Channel::new(
         String::new(),
         String::new(),
         None,
         None
      );
      
      //Add user to channelpool
      for channel in channelpool.lock().unwrap().iter_mut() {
         if channel.id.contains(v[0]) { 
            if channel.users.is_none() {
               let mut v = vec![];
               v.push(user);
               channel.users.insert(v);
               current_channel = channel.clone();
            } else {
               channel.users.as_mut().unwrap().push(user); 
               current_channel = channel.clone();
            } 
            break;
         }     
      };

      //Send old chat messages to client that's connecting
      let mut writer = main_stream.try_clone().expect("Can't clone main_stream");
      if !current_channel.chat_msgs.is_none() {
         let serialized = bincode::serialize(&current_channel.chat_msgs.as_ref().unwrap()).unwrap();
         writer.write(&serialized).expect("Cant write to client");
         current_channel.chat_msgs.take();
      }

      std::thread::spawn( move || { 
         let (tx, rx) = std::sync::mpsc::channel(); 
         //Updating thread; keeps track of the user len and chat len in the current channel.
         std::thread::spawn( move || { 
            let mut current_users_len = 0;
            let mut current_chat_len = 0;

            loop { 
               match rx.try_recv() { 
                  Ok(_) => {
                     println!("     Exiting updating_thread");
                     break;
                  },
                  Err(TryRecvError::Disconnected) => {
                     println!("      Channel disconnected");
                     break;
                  },
                  Err(TryRecvError::Empty) => { 
                     for chnl in channelpool.lock().unwrap().iter_mut() {  
                        if current_channel.id.contains(&chnl.id) { 
                           if current_users_len != chnl.users.as_ref().unwrap().len() {
                              signal_client(&updater_stream, String::from("UPDATEUSERS"));
                              current_users_len = chnl.users.as_ref().unwrap().len();
                              break;
                           }
                           else if current_chat_len != chnl.chat_msgs.as_ref().unwrap().len() { 
                              for users in chnl.users.as_ref() {
                                 for user in users {
                                    for con in connectionpool.lock().unwrap().iter() {
                                       if user.updater_stream.to_string().contains( 
                                          &con.updater_stream.as_ref().unwrap().peer_addr().unwrap().to_string()) {
                                             signal_client(&con.updater_stream.as_ref().unwrap(),
                                             String::from("UPDATECHAT"));
                                       } 
                                    }
                                 }
                              }
                              if current_chat_len == CHAT_MAX_SIZE {
                                 chnl.chat_msgs.as_mut().unwrap().remove(0); 
                              }
                              current_chat_len = chnl.chat_msgs.as_ref().unwrap().len();
                              break;
                           }
                        } 
                     }
                  }, 
               } 
            }
         });

         //Chatting DEPRICATED 
         let mut reader = std::io::BufReader::new(chat_stream);
         let mut line = String::new(); 
         loop {
            let result = reader.read_line(&mut line).unwrap();
            line.clear();

            if result == 0 {
               println!("     Exiting chat thread.. ");
               break;
            }
         }
         println!("     Chat thread exited");
         let _ = tx.send(());

      });
   println!("  Exiting main_thread..."); 
}

fn send_channel_info(
   stream: &TcpStream,
   channelpool: &Arc<Mutex<Vec<Channel>>>) {
      let pool = channelpool.clone();

      // checks if channelpool contains channel id that "catch_signal" returns
      let result = catch_signal(stream);
      let pool_result = || -> bool {
         let mut found = false;
         for c in pool.lock().unwrap().iter() {
            if result.contains(&c.id) { 
               found = true; 
               break;
            }
         }
         found
      };
      let mut writer = std::io::BufWriter::new(stream.try_clone()
         .expect("clone failed..."));

      // if its true, it creates new, blank channel... 
      if pool_result() { 
         let mut unserialized = Channel::new(
            String::new(),
            String::new(),
            None,
            None
         );

         // ...then clones the fields with the found one...
         for c in pool.lock().unwrap().iter()  {
            if result.contains(&c.id) { 
               unserialized = Channel {
                  id: c.id.clone(),
                  channel_name: c.channel_name.clone(),
                  users: None,
                  chat_msgs: None,
               }; 
            }
         };

         // ...then serializes the channel and sends it back to client 
         let serialized = bincode::serialize(&unserialized).unwrap();
         writer.write(&serialized).expect("Cant write to client");
      } else { 
         let buf = [0; 255];
         writer.write(&buf).expect("Cant write to client");
      }
   } 
      
fn update_channel_users(
   stream: &TcpStream, 
   channelpool: &Arc<Mutex<Vec<Channel>>>,
   connectionpool: &Arc<Mutex<Vec<Connection>>>) { 
      let channel = catch_signal(stream);

      for chnl in channelpool.lock().unwrap().iter() {
         if chnl.id.contains(&channel) {
            for con in connectionpool.lock().unwrap().iter() {
                  if con.main_stream.as_ref().unwrap().peer_addr()
                     .unwrap()
                     .to_string()
                     .contains(&stream.peer_addr().unwrap().to_string()) {
                        //Serialise only user's name and id
                        let mut vec = vec![];
                        for user in chnl.users.clone().unwrap() {
                           let user = (user.name, user.id);
                           vec.push(user);
                        }
                        let serialized = bincode::serialize(&vec).unwrap();
                        if !con.chat_stream.is_none() {
                              con.main_stream.as_ref().unwrap().write(&serialized).expect("Cant write to client");
                              con.main_stream.as_ref().unwrap().flush().unwrap(); 
                        }
                  }
               } 
         break;
         }
      }
}

fn disconnect(
   stream: &TcpStream, 
   channelpool: &Arc<Mutex<Vec<Channel>>>,
   connectionpool: &Arc<Mutex<Vec<Connection>>>) {

      let mut connectionpool = connectionpool.lock().unwrap();
      let connection = stream.peer_addr().unwrap().to_string();

      let channel_id = catch_signal(stream);
      for con in connectionpool.iter_mut() {
         if con.main_stream.as_ref().unwrap().peer_addr()
         .unwrap()
         .to_string()
         .contains(&connection) {   
            //TODO better looppings
            for channel in channelpool.lock().unwrap().iter_mut() {
               if channel.id.contains(&channel_id) {
                  for users in channel.users.as_mut() {
                     for user in users.iter_mut() {
                        if user.main_stream.to_string().contains(&connection) { 
                           let index = users.iter().position(
                              | x | x.main_stream.to_string().contains(&connection)).unwrap();
                           con.chat_stream.as_ref().unwrap().shutdown(std::net::Shutdown::Both)
                              .expect("Something wronk");
                           con.chat_stream.take();
                           users.remove(index);
                           if users.len() == 0 {
                              channel.chat_msgs.take();
                           }
                           break;
                        } 
                     }
                  }
               }
            }
         } 
      }  
}

fn catch_signal(stream: &TcpStream) -> String { 
   let mut reader = std::io::BufReader::new(stream);
   let mut line = String::new();
   reader.read_line(&mut line).unwrap();
   line.pop();
   line
}

fn signal_client(stream: &TcpStream, mut line: String) {
   let mut writer = std::io::BufWriter::new(stream);
   line.push('\n');
   let line = line.as_bytes();
   writer.write(line).unwrap();
}
