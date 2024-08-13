use std::{io::Read, time::Duration};

use anyhow::Context;
use lapin::{
    options::{BasicPublishOptions, BasicQosOptions, QueueDeclareOptions},
    types::FieldTable,
    BasicProperties, Channel, Connection, ConnectionProperties, Queue,
};
use serde::{Deserialize, Serialize};

async fn download_and_unzip(url: &str) -> Result<Vec<String>, anyhow::Error> {
    let client = reqwest::Client::new();
    let res = client.get(url).send().await.unwrap();
    match res.status() {
        reqwest::StatusCode::OK => {
            let body = res.bytes().await.unwrap();
            let mut decoder = flate2::read::GzDecoder::new(&body[..]);
            let mut buffer = Vec::new();
            decoder.read_to_end(&mut buffer).unwrap();
            let paths = String::from_utf8(buffer).unwrap();
            Ok(paths.lines().map(|s| s.to_string()).collect())
        }
        _ => {
            println!("Failed to fetch the index file");
            Err(anyhow::anyhow!("Failed to fetch the index file"))
        }
    }
}

const BATCH_SIZE: usize = 1000;
pub const CC_QUEUE_NAME: &str = "batches";
const RABBIT_MQ_TIMEOUT: Duration = Duration::from_secs(20);

pub fn get_rabbitmq_connection_string() -> String {
    std::env::var("RABBITMQ_CONNECTION_STRING").expect("RABBITMQ_CONNECTION_STRING must be set.")
}

pub async fn rabbitmq_connection() -> Result<Connection, anyhow::Error> {
    let connection_string = get_rabbitmq_connection_string();
    let connection = tokio::time::timeout(
        RABBIT_MQ_TIMEOUT,
        Connection::connect(&connection_string, ConnectionProperties::default()),
    )
    .await
    .context("Timed out while trying to connect to RabbitMQ")??;
    Ok(connection)
}

pub async fn rabbitmq_channel_with_queue(
    conn: &Connection,
    queue_name: &str,
) -> Result<(Channel, Queue), anyhow::Error> {
    let channel = rabbitmq_channel(conn).await?;
    let queue = rabbitmq_declare_queue(&channel, queue_name, FieldTable::default()).await?;
    Ok((channel, queue))
}

pub async fn rabbitmq_declare_queue(
    channel: &Channel,
    queue_name: &str,
    arguments: FieldTable,
) -> Result<Queue, anyhow::Error> {
    let queue = tokio::time::timeout(
        RABBIT_MQ_TIMEOUT,
        channel.queue_declare(queue_name, QueueDeclareOptions::default(), arguments),
    )
    .await
    .context("Timed out while trying to declare a RabbitMQ queue")?
    .context("Failed to declare RabbitMQ queue")?;

    Ok(queue)
}

pub async fn rabbitmq_channel(conn: &Connection) -> Result<Channel, anyhow::Error> {
    let channel = tokio::time::timeout(RABBIT_MQ_TIMEOUT, conn.create_channel())
        .await
        .context("Timed out while trying to create a RabbitMQ channel")?
        .context("Failed to create RabbitMQ channel")?;

    tokio::time::timeout(
        RABBIT_MQ_TIMEOUT,
        channel.basic_qos(1, BasicQosOptions::default()),
    )
    .await
    .context("Timed out while trying to set QoS on the channel")?
    .context("Failed to set QoS on the channel")?;
    Ok(channel)
}

#[tokio::main]
async fn main() {
    let rabbit_conn = rabbitmq_connection().await.unwrap();
    let (channel, _queue) = rabbitmq_channel_with_queue(&rabbit_conn, CC_QUEUE_NAME)
        .await
        .unwrap();

    let paths = download_and_unzip(
        "https://data.commoncrawl.org/crawl-data/CC-MAIN-2024-30/cc-index.paths.gz",
    )
    .await
    .unwrap();
    for path in paths {
        if path.contains("cdx-") {
            let english_cdx_entries =
                download_and_unzip(&format!("https://data.commoncrawl.org/{path}"))
                    .await
                    .unwrap()
                    .iter()
                    .map(|s| parse_cdx_line(s))
                    .filter(|e| {
                        if let Some(languages) = e.metadata.languages.as_ref() {
                            languages.contains("eng")
                        } else {
                            false
                        }
                    })
                    .collect::<Vec<_>>();
            for batch in english_cdx_entries.as_slice().chunks(BATCH_SIZE) {
                println!("Sending a batch of {} entries", batch.len());
                channel
                    .basic_publish(
                        "",
                        CC_QUEUE_NAME,
                        BasicPublishOptions::default(),
                        &serde_json::to_vec(&batch).unwrap(),
                        BasicProperties::default(),
                    )
                    .await
                    .context("rabbitmq basic publish")
                    .unwrap();
            }
            break;
        }
        println!("{}", path);
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct CdxMetadata {
    url: String,
    status: String,
    length: String,
    offset: String,
    filename: String,
    languages: Option<String>,
}

#[derive(Debug, Serialize)]
struct CdxEntry {
    surt_url: String,
    timestamp: String,
    metadata: CdxMetadata,
}

fn parse_cdx_line(line: &str) -> CdxEntry {
    let mut parts = line.splitn(3, ' ');
    CdxEntry {
        surt_url: parts.next().unwrap().to_string(),
        timestamp: parts.next().unwrap().to_string(),
        metadata: serde_json::from_str(parts.next().unwrap()).unwrap(),
    }
}

#[cfg(test)]
mod tests {
    use crate::parse_cdx_line;

    #[test]
    fn can_parse_cdx_file() {
        let content = r#"0,100,22,165)/ 20240722120756 {"url": "http://165.22.100.0/", "mime": "text/html", "mime-detected": "text/html", "status": "301", "digest": "DCNYNIFG5SBRCVS5PCUY4YY2UM2WAQ4R", "length": "689", "offset": "3499", "filename": "crawl-data/CC-MAIN-2024-30/segments/1720763517846.73/crawldiagnostics/CC-MAIN-20240722095039-20240722125039-00443.warc.gz", "redirect": "https://157.245.55.71/"}
0,100,22,165)/robots.txt 20240722120755 {"url": "http://165.22.100.0/robots.txt", "mime": "text/html", "mime-detected": "text/html", "status": "301", "digest": "LYEE2BXON4MCQCP5FDVDNILOWBKCZZ6G", "length": "700", "offset": "4656", "filename": "crawl-data/CC-MAIN-2024-30/segments/1720763517846.73/robotstxt/CC-MAIN-20240722095039-20240722125039-00410.warc.gz", "redirect": "https://157.245.55.71/robots.txt"}
0,100,59,139)/ 20240723213521 {"url": "https://139.59.100.0/", "mime": "text/html", "mime-detected": "text/html", "status": "200", "digest": "5JOQMMSNM6N7UCLGGYXDSPSB3FYAQS2C", "length": "16650", "offset": "64016172", "filename": "crawl-data/CC-MAIN-2024-30/segments/1720763518115.82/warc/CC-MAIN-20240723194208-20240723224208-00279.warc.gz", "charset": "UTF-8", "languages": "ind,eng"}"#;
        let cdx: Vec<_> = content.lines().map(parse_cdx_line).collect();
        assert_eq!(cdx.len(), 3);
    }
}
