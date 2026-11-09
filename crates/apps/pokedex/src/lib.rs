//! A random Pokémon, from <https://pokeapi.co>.
//!
//! The point of this app is the request. `pokeapi.co` sends no CORS headers, so a
//! `fetch` from here would be readable only if it did — the browser refuses to hand
//! over the body. So the request goes through the shell's server instead, which is
//! allowed to talk to `pokeapi.co` when `WEB_DESKTOP_PROXY_HOSTS=pokeapi.co`.
//!
//! The sprite is a different story, and deliberately so: an `<img>` needs no CORS, so
//! it loads straight from the sprite CDN. Only *reading* a response needs permission.

use std::rc::Rc;
use std::sync::Arc;

use app_sdk::prelude::*;
use leptos::task::spawn_local;
use serde::Deserialize;

/// `GET /api/v2/pokemon` — the list of everything the API will answer for.
///
/// Rolling an id against `count` does not work: the count includes forms that are not
/// addressable (`/pokemon/1026/` is a 404), so a quarter of rolls would come back empty.
/// The list carries the url for each entry, which always resolves.
#[derive(Debug, Clone, PartialEq, Deserialize)]
struct Index {
    results: Vec<Entry>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
struct Entry {
    name: String,
    url: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
struct Pokemon {
    id: u32,
    name: String,
    /// Decimetres.
    #[serde(default)]
    height: u32,
    /// Hectograms.
    #[serde(default)]
    weight: u32,
    #[serde(default)]
    types: Vec<TypeSlot>,
    #[serde(default)]
    stats: Vec<Stat>,
    #[serde(default)]
    sprites: Sprites,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
struct TypeSlot {
    #[serde(rename = "type")]
    kind: Named,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
struct Stat {
    base_stat: u32,
    stat: Named,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
struct Named {
    name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
struct Sprites {
    front_default: Option<String>,
    #[serde(default)]
    other: Option<Other>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
struct Other {
    #[serde(rename = "official-artwork")]
    official_artwork: Artwork,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
struct Artwork {
    front_default: Option<String>,
}

impl Pokemon {
    /// The prettiest picture the API offers, falling back to the small sprite.
    fn art(&self) -> Option<&str> {
        self.sprites
            .other
            .as_ref()
            .and_then(|other| other.official_artwork.front_default.as_deref())
            .or(self.sprites.front_default.as_deref())
    }

    fn title(&self) -> String {
        let mut title = self.name.clone();
        if let Some(first) = title.get_mut(0..1) {
            first.make_ascii_uppercase();
        }
        title
    }
}

/// Colour per type, so the chips read at a glance.
fn type_colour(kind: &str) -> &'static str {
    match kind {
        "fire" => "#f97316",
        "water" => "#38bdf8",
        "grass" => "#4ade80",
        "electric" => "#facc15",
        "ice" => "#a5f3fc",
        "fighting" => "#ef4444",
        "poison" => "#c084fc",
        "ground" => "#d6a35c",
        "flying" => "#a5b4fc",
        "psychic" => "#f472b6",
        "bug" => "#a3e635",
        "rock" => "#b8a06a",
        "ghost" => "#8b7fd4",
        "dragon" => "#7c6cf0",
        "dark" => "#6b7280",
        "steel" => "#9aa5b1",
        "fairy" => "#f9a8d4",
        _ => "#8b93a7",
    }
}

#[derive(Clone, PartialEq)]
enum State {
    Loading,
    Ready(Box<Pokemon>),
    Failed(String),
}

async fn index() -> Result<Vec<Entry>, String> {
    let index: Index =
        app_sdk::fetch::json("https://pokeapi.co/api/v2/pokemon?limit=100000").await?;
    Ok(index.results)
}

/// Fetch one entry, retrying once: the proxy makes an upstream hiccup look like a
/// failed roll, and there is nothing to lose by asking again.
async fn fetch_pokemon(url: &str) -> Result<Pokemon, String> {
    match app_sdk::fetch::json(url).await {
        Ok(pokemon) => Ok(pokemon),
        Err(first) => app_sdk::fetch::json(url).await.map_err(|_| first),
    }
}

const STYLES: &str = "
.pokedex { display: flex; flex-direction: column; gap: 12px; padding: 14px; height: 100%;
           box-sizing: border-box; align-items: center; text-align: center; font-family: inherit; }
.poke-art { width: 190px; height: 190px; object-fit: contain; image-rendering: auto;
            filter: drop-shadow(0 8px 20px rgba(0,0,0,.45)); }
.poke-name { margin: 0; font-size: 22px; font-weight: 700; letter-spacing: .01em; }
.poke-id { font-family: ui-monospace, monospace; font-size: 12px; opacity: .65; }
.poke-types { display: flex; gap: 6px; justify-content: center; flex-wrap: wrap; }
.poke-chip { padding: 3px 10px; border-radius: 999px; font-size: 11.5px; font-weight: 600;
             color: #0b0d12; text-transform: capitalize; }
.poke-meta { display: flex; gap: 18px; font-family: ui-monospace, monospace; font-size: 12px; }
.poke-stats { display: grid; grid-template-columns: auto 1fr auto; gap: 5px 10px; width: 100%;
              max-width: 320px; align-items: center; font-size: 11.5px; }
.poke-stat-bar { display: block; height: 6px; border-radius: 3px;
                 background: rgba(255,255,255,.12); overflow: hidden; }
/* `display: block` matters: the fill is a span, and an inline box ignores width and
   height, so the bars render empty however sensible the inline style looks. */
.poke-stat-fill { display: block; height: 100%; border-radius: 3px; }
.poke-stat-name { text-transform: uppercase; letter-spacing: .06em; opacity: .7; font-size: 10px; }
.poke-foot { margin-top: auto; display: flex; gap: 8px; align-items: center; }
.poke-btn { padding: 9px 16px; border-radius: 10px; border: 1px solid; font-size: 12.5px;
            font-weight: 600; cursor: pointer; font-family: inherit; }
.poke-note { font-size: 11.5px; line-height: 1.5; opacity: .75; max-width: 40ch; }
.poke-error { font-size: 12px; line-height: 1.55; color: #ff9ea1; max-width: 42ch; }
";

app_sdk::app! {
    fn pokedex(ctx: Context) -> impl IntoView {
        let theme = ctx.theme();
        let accent = theme.accent.clone();
        let state = RwSignal::new(State::Loading);
        let ctx_roll = ctx.clone();

        // The list of names is fetched once per window and kept, so a roll costs one
        // request instead of two.
        // `Arc`, not `Rc`: a signal's value has to be `Send + Sync`.
        let names = RwSignal::new(None::<Arc<Vec<Entry>>>);

        let roll: Rc<dyn Fn()> = Rc::new(move || {
            state.set(State::Loading);
            let ctx = ctx_roll.clone();
            let cached = names.get_untracked();
            spawn_local(async move {
                let entries = match cached {
                    Some(entries) => Ok(entries),
                    None => match index().await {
                        Ok(entries) => {
                            let entries = Arc::new(entries);
                            names.set(Some(Arc::clone(&entries)));
                            Ok(entries)
                        }
                        Err(err) => Err(err),
                    },
                };
                let outcome = match entries {
                    Ok(entries) if !entries.is_empty() => {
                        // `Math::random` is the browser's own randomness: no dependency
                        // needed for one die.
                        let pick =
                            (js_sys::Math::random() * entries.len() as f64) as usize % entries.len();
                        fetch_pokemon(&entries[pick].url).await
                    }
                    Ok(_) => Err("the API returned no Pokémon".to_owned()),
                    Err(err) => Err(err),
                };
                match outcome {
                    Ok(pokemon) => {
                        ctx.log(&format!("rolled {}", pokemon.name));
                        state.set(State::Ready(Box::new(pokemon)));
                    }
                    Err(err) => state.set(State::Failed(err)),
                }
            });
        });

        // One Pokémon as soon as the window opens.
        let first = Rc::clone(&roll);
        Effect::new(move |_| first());

        let again = Rc::clone(&roll);

        let text = theme.text.clone();
        let muted = theme.muted.clone();
        let stroke = theme.stroke.clone();
        let panel = theme.panel.clone();

        view! {
            <style>{STYLES}</style>
            <div class="pokedex" style=format!("color:{text};")>
                {move || match state.get() {
                    State::Loading => view! {
                        <p class="poke-note" style=format!("color:{muted};")>"Rolling…"</p>
                    }.into_any(),
                    State::Failed(err) => view! {
                        <p class="poke-error">"Could not reach pokeapi.co: " {err}</p>
                        <p class="poke-note" style=format!("color:{muted};")>
                            "The request is made by the server, so pokeapi.co has to be allowed there:"
                        </p>
                        <code class="poke-note">"WEB_DESKTOP_PROXY_HOSTS=pokeapi.co"</code>
                    }.into_any(),
                    State::Ready(pokemon) => {
                        let name = pokemon.title();
                        let dex = format!("#{:03}", pokemon.id);
                        let art = pokemon.art().map(str::to_owned);
                        let height = format!("{:.1} m", f64::from(pokemon.height) / 10.0);
                        let weight = format!("{:.1} kg", f64::from(pokemon.weight) / 10.0);
                        let types = pokemon.types.iter()
                            .map(|slot| (slot.kind.name.clone(), type_colour(&slot.kind.name)))
                            .collect::<Vec<_>>();
                        let stats = pokemon.stats.iter()
                            .map(|stat| (stat.stat.name.clone(), stat.base_stat))
                            .collect::<Vec<_>>();
                        view! {
                            {art.map(|url| view! {
                                <img class="poke-art" src=url alt="" />
                            })}
                            <h2 class="poke-name">{name}</h2>
                            <span class="poke-id">{dex}</span>
                            <div class="poke-types">
                                {types.into_iter().map(|(kind, colour)| view! {
                                    <span class="poke-chip" style=format!("background:{colour};")>{kind}</span>
                                }).collect_view()}
                            </div>
                            <div class="poke-meta" style=format!("color:{muted};")>
                                <span>{height}</span>
                                <span>{weight}</span>
                            </div>
                            <div class="poke-stats">
                                {stats.into_iter().map(|(stat, value)| {
                                    let width = format!("{:.0}%", (f64::from(value) / 160.0 * 100.0).min(100.0));
                                    view! {
                                        <span class="poke-stat-name" style=format!("color:{muted};")>{stat}</span>
                                        <span class="poke-stat-bar">
                                            <span class="poke-stat-fill" style=format!("width:{width};background:{accent};")></span>
                                        </span>
                                        <span>{value}</span>
                                    }
                                }).collect_view()}
                            </div>
                        }.into_any()
                    }
                }}

                <div class="poke-foot">
                    <button
                        class="poke-btn"
                        style=format!("border-color:{stroke};background:{panel};color:{text};")
                        on:click=move |_| again()
                    >
                        "Random again"
                    </button>
                </div>
            </div>
        }
    }
}
