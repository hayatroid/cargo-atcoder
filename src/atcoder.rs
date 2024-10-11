use crate::http::{is_http_error, Client};
use anyhow::{anyhow, bail, Context as _, Result};
use itertools::Itertools as _;
use scraper::{element_ref::ElementRef, Html, Selector};
use std::fmt;
use std::path::Path;
use url::Url;

const ATCODER_ENDPOINT: &str = "https://atcoder.jp";

pub struct AtCoder {
    client: Client,
}

#[derive(Debug)]
pub struct ContestInfo {
    problems: Vec<Problem>,
}

#[derive(Debug)]
pub struct Problem {
    pub id: String,
    pub _name: String,
    pub url: String,
    pub _tle: String,
    pub _mle: String,
}

#[derive(Debug, Clone)]
pub struct TestCase {
    pub input: String,
    pub output: String,
}

impl ContestInfo {
    pub fn problem(&self, id: &str) -> Option<&Problem> {
        self.problems
            .iter()
            .find(|p| p.id.to_lowercase() == id.to_lowercase())
    }

    pub fn problem_ids_lowercase(&self) -> Vec<String> {
        self.problems.iter().map(|p| p.id.to_lowercase()).collect()
    }
}

impl AtCoder {
    pub fn new(session_file: &Path) -> Result<AtCoder> {
        Ok(Self {
            client: Client::new(session_file, ATCODER_ENDPOINT)?,
        })
    }

    async fn check_login(&self) -> Result<()> {
        let _ = self
            .username()
            .await?
            .with_context(|| "You are not logged in. Please login first.")?;
        Ok(())
    }

    pub async fn username(&self) -> Result<Option<String>> {
        let doc = self.http_get("/").await?;
        let doc = Html::parse_document(&doc);

        let r = doc
            .select(&Selector::parse("li a[href^=\"/users/\"]").unwrap())
            .next();

        if r.is_none() {
            return Ok(None);
        }

        Ok(Some(
            r.unwrap().value().attr("href").unwrap()[7..].to_owned(),
        ))
    }

    pub async fn login(&self, username: &str, password: &str) -> Result<()> {
        let document = self.http_get("/login").await?;
        let document = Html::parse_document(&document);

        let csrf_token = document
            .select(&Selector::parse("input[name=\"csrf_token\"]").unwrap())
            .next()
            .with_context(|| "cannot find csrf_token")?;

        let csrf_token = csrf_token
            .value()
            .attr("value")
            .with_context(|| "cannot find csrf_token")?;

        let res = self
            .http_post_form(
                "/login",
                &[
                    ("username", username),
                    ("password", password),
                    ("csrf_token", csrf_token),
                ],
            )
            .await?;

        let res = Html::parse_document(&res);

        // On failure:
        // <div class="alert alert-danger alert-dismissible col-sm-12 fade in" role="alert">
        //   ...
        //   {{error message}}
        // </div>
        if let Some(err) = res
            .select(&Selector::parse("div.alert-danger").unwrap())
            .next()
        {
            bail!(
                "Login failed: {}",
                err.last_child().unwrap().value().as_text().unwrap().trim()
            );
        }

        // On success:
        // <div class="alert alert-success alert-dismissible col-sm-12 fade in" role="alert" >
        //     ...
        //     ようこそ、tanakh さん。
        // </div>
        if res
            .select(&Selector::parse("div.alert-success").unwrap())
            .next()
            .is_some()
        {
            return Ok(());
        }

        Err(anyhow!("Login failed: Unknown error"))
    }

    pub async fn problem_ids_from_score_table(
        &self,
        contest_id: &str,
    ) -> Result<Option<Vec<String>>> {
        let doc = self.http_get(&format!("/contests/{}", contest_id)).await?;

        Html::parse_document(&doc)
            .select(&Selector::parse("#contest-statement > .lang > .lang-ja table").unwrap())
            .filter(|table| {
                let header = table
                    .select(&Selector::parse("thead > tr > th").unwrap())
                    .flat_map(|r| r.text())
                    .collect::<Vec<_>>();
                header == ["Task", "Score"] || header == ["問題", "点数"]
            })
            .exactly_one()
            .ok()
            .map(|table| {
                table
                    .select(&Selector::parse("tbody > tr").unwrap())
                    .map(|tr| {
                        let text = tr
                            .select(&Selector::parse("td").unwrap())
                            .flat_map(|r| r.text())
                            .collect::<Vec<_>>();
                        match text.len() {
                            2 => Ok(text[0].to_owned()),
                            _ => Err(anyhow!("could not parse the table")),
                        }
                    })
                    .collect()
            })
            .transpose()
    }

    pub async fn contest_info(&self, contest_id: &str) -> Result<ContestInfo> {
        let doc = self
            .retrieve_text_or_error_message(&format!("/contests/{}/tasks", contest_id), || {
                format!(
                    "You are not participating in `{}`, or it does not yet exist",
                    contest_id,
                )
            })
            .await?;

        let doc = Html::parse_document(&doc);
        let sel_problem = Selector::parse("table tbody tr").unwrap();

        let mut problems = vec![];

        for row in doc.select(&sel_problem) {
            let sel_td = Selector::parse("td").unwrap();
            let mut it = row.select(&sel_td);
            let c1 = it.next().unwrap();
            let c2 = it.next().unwrap();
            let c3 = it.next().unwrap();
            let c4 = it.next().unwrap();

            let id = c1
                .select(&Selector::parse("a").unwrap())
                .next()
                .unwrap()
                .inner_html();

            let name = c2
                .select(&Selector::parse("a").unwrap())
                .next()
                .unwrap()
                .inner_html();

            let url = c2
                .select(&Selector::parse("a").unwrap())
                .next()
                .unwrap()
                .value()
                .attr("href")
                .unwrap();

            let tle = c3.inner_html();
            let mle = c4.inner_html();

            problems.push(Problem {
                id: id.trim().to_owned(),
                _name: name.trim().to_owned(),
                url: url.trim().to_owned(),
                _tle: tle.trim().to_owned(),
                _mle: mle.trim().to_owned(),
            });
        }

        Ok(ContestInfo { problems })
    }

    pub async fn test_cases(&self, problem_url: &str) -> Result<Vec<TestCase>> {
        let doc = self.http_get(problem_url).await?;

        let doc = Html::parse_document(&doc);

        let h3_sel = Selector::parse("h3").unwrap();

        let mut inputs_ja = vec![];
        let mut outputs_ja = vec![];
        let mut inputs_en = vec![];
        let mut outputs_en = vec![];

        for r in doc.select(&h3_sel) {
            let p = ElementRef::wrap(r.parent().unwrap()).unwrap();
            let label = p.select(&h3_sel).next().unwrap().inner_html();
            let label = label.trim();
            // dbg!(r.parent().unwrap().first_child().unwrap().value());

            // let label = r
            //     .prev_sibling()
            //     .unwrap()
            //     .first_child()
            //     .unwrap()
            //     .value()
            //     .as_text()
            //     .unwrap();

            let f = || {
                p.select(&Selector::parse("pre").unwrap())
                    .next()
                    .unwrap()
                    .text()
                    .exactly_one()
                    .map(|s| s.trim().to_owned())
                    .unwrap_or_default()
            };
            if label.starts_with("入力例") {
                inputs_ja.push(f());
            }
            if label.starts_with("出力例") {
                outputs_ja.push(f());
            }

            if label.starts_with("Sample Input") {
                inputs_en.push(f());
            }
            if label.starts_with("Sample Output") {
                outputs_en.push(f());
            }
        }

        let (inputs, outputs) = if !inputs_ja.is_empty() && inputs_ja.len() == outputs_ja.len() {
            (inputs_ja, outputs_ja)
        } else if !inputs_en.is_empty() && inputs_en.len() == outputs_en.len() {
            (inputs_en, outputs_en)
        } else {
            bail!(
                "Could not scrape sample test cases (JA inputs: {}, JA outputs: {}, EN inputs: \
                 {}, EN outputs: {})",
                inputs_ja.len(),
                outputs_ja.len(),
                inputs_en.len(),
                outputs_en.len(),
            );
        };

        let mut ret = vec![];
        for i in 0..inputs.len() {
            ret.push(TestCase {
                input: inputs[i].clone(),
                output: outputs[i].clone(),
            });
        }
        Ok(ret)
    }

    pub async fn submit(
        &self,
        contest_id: &str,
        problem_id: &str,
        source_code: &str,
    ) -> Result<()> {
        self.check_login().await?;

        let doc = self
            .http_get(&format!("/contests/{}/submit", contest_id))
            .await?;

        let (task_screen_name, language_id, language_name, csrf_token) = {
            let doc = Html::parse_document(&doc);

            let task_screen_name = (|| {
                for r in doc.select(
                    &Selector::parse("select[name=\"data.TaskScreenName\"] option").unwrap(),
                ) {
                    if r.inner_html()
                        .split_whitespace()
                        .next()
                        .unwrap()
                        .to_lowercase()
                        .starts_with(&problem_id.to_lowercase())
                    {
                        return Ok(r.value().attr("value").unwrap());
                    }
                }
                Err(anyhow!("Problem not found: {}", problem_id))
            })()?;

            let (language_id, language_name) = (|| {
                for r in doc.select(
                    &Selector::parse(&format!(
                        "div[id=\"select-lang-{}\"] select option",
                        &task_screen_name
                    ))
                    .unwrap(),
                ) {
                    if r.inner_html()
                        .split_whitespace()
                        .next()
                        .unwrap_or("")
                        .to_lowercase()
                        .starts_with("rust")
                    {
                        return Ok((r.value().attr("value").unwrap(), r.inner_html()));
                    }
                }
                Err(anyhow!(
                    "Rust seems to be not available in problem {}...",
                    problem_id
                ))
            })()?;

            let csrf_token = doc
                .select(&Selector::parse("input[name=\"csrf_token\"]").unwrap())
                .next()
                .unwrap()
                .value()
                .attr("value")
                .unwrap();

            (
                task_screen_name.to_owned(),
                language_id.to_owned(),
                language_name,
                csrf_token.to_owned(),
            )
        };

        let _ = self
            .http_post_form(
                &format!("/contests/{}/submit", contest_id),
                &[
                    ("data.TaskScreenName", &task_screen_name),
                    ("data.LanguageId", &language_id),
                    ("sourceCode", source_code),
                    ("csrf_token", &csrf_token),
                ],
            )
            .await?;

        println!(
            "Submitted to problem `{}`, using language `{}`",
            task_screen_name, language_name
        );
        Ok(())
    }

    async fn retrieve_text_or_error_message<T: fmt::Display, F: FnOnce() -> T>(
        &self,
        path: &str,
        context_on_logged_in: F,
    ) -> Result<String> {
        match self.http_get(path).await {
            Err(err) if is_http_error(&err, reqwest::StatusCode::NOT_FOUND) => {
                Err(match self.username().await {
                    Ok(username) => err.context(if username.is_some() {
                        anyhow!("{}", context_on_logged_in())
                    } else {
                        anyhow!("You are not logged in. Please login first")
                    }),
                    Err(err) => err,
                })?
            }
            ret => Ok(ret?),
        }
    }

    async fn http_get(&self, path: &str) -> Result<String> {
        self.client
            .get(&format!("{}{}", ATCODER_ENDPOINT, path).parse::<Url>()?)
            .await
    }

    async fn http_post_form(&self, path: &str, form: &[(&str, &str)]) -> Result<String> {
        self.client
            .post_form(
                &format!("{}{}", ATCODER_ENDPOINT, path).parse::<Url>()?,
                form,
            )
            .await
    }
}
