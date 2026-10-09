//! Google's country codes, in the spelling the rest of Micaforge uses.
//!
//! Search Console names a country by its ISO 3166-1 alpha-3 code in lower case
//! (`usa`, `deu`), and every other table here stores alpha-2 (`US`, `DE`),
//! which is also what the dashboard turns into a country's name. The sync
//! converts on the way in, so a report never has to know two spellings.
//! Google's `zzz` (it could not tell) becomes empty, the same "unknown" the
//! other tables keep; `xkk` is Kosovo, which has no ISO code and is `XK` by
//! the common convention.

/// `alpha-3 alpha-2` pairs, alpha-3 order.
const PAIRS: &str = "\
abw AW afg AF ago AO aia AI ala AX alb AL and AD are AE arg AR arm AM asm AS ata AQ \
atf TF atg AG aus AU aut AT aze AZ bdi BI bel BE ben BJ bes BQ bfa BF bgd BD bgr BG \
bhr BH bhs BS bih BA blm BL blr BY blz BZ bmu BM bol BO bra BR brb BB brn BN btn BT \
bvt BV bwa BW caf CF can CA cck CC che CH chl CL chn CN civ CI cmr CM cod CD cog CG \
cok CK col CO com KM cpv CV cri CR cub CU cuw CW cxr CX cym KY cyp CY cze CZ deu DE \
dji DJ dma DM dnk DK dom DO dza DZ ecu EC egy EG eri ER esh EH esp ES est EE eth ET \
fin FI fji FJ flk FK fra FR fro FO fsm FM gab GA gbr GB geo GE ggy GG gha GH gib GI \
gin GN glp GP gmb GM gnb GW gnq GQ grc GR grd GD grl GL gtm GT guf GF gum GU guy GY \
hkg HK hmd HM hnd HN hrv HR hti HT hun HU idn ID imn IM ind IN iot IO irl IE irn IR \
irq IQ isl IS isr IL ita IT jam JM jey JE jor JO jpn JP kaz KZ ken KE kgz KG khm KH \
kir KI kna KN kor KR kwt KW lao LA lbn LB lbr LR lby LY lca LC lie LI lka LK lso LS \
ltu LT lux LU lva LV mac MO maf MF mar MA mco MC mda MD mdg MG mdv MV mex MX mhl MH \
mkd MK mli ML mlt MT mmr MM mne ME mng MN mnp MP moz MZ mrt MR msr MS mtq MQ mus MU \
mwi MW mys MY myt YT nam NA ncl NC ner NE nfk NF nga NG nic NI niu NU nld NL nor NO \
npl NP nru NR nzl NZ omn OM pak PK pan PA pcn PN per PE phl PH plw PW png PG pol PL \
pri PR prk KP prt PT pry PY pse PS pyf PF qat QA reu RE rou RO rus RU rwa RW sau SA \
sdn SD sen SN sgp SG sgs GS shn SH sjm SJ slb SB sle SL slv SV smr SM som SO spm PM \
srb RS ssd SS stp ST sur SR svk SK svn SI swe SE swz SZ sxm SX syc SC syr SY tca TC \
tcd TD tgo TG tha TH tjk TJ tkl TK tkm TM tls TL ton TO tto TT tun TN tur TR tuv TV \
twn TW tza TZ uga UG ukr UA umi UM ury UY usa US uzb UZ vat VA vct VC ven VE vgb VG \
vir VI vnm VN vut VU wlf WF wsm WS xkk XK yem YE zaf ZA zmb ZM zwe ZW";

/// The alpha-2 code for Google's alpha-3 one, or empty when there is none.
pub fn alpha2(google: &str) -> String {
    let wanted = google.trim().to_ascii_lowercase();
    if wanted.len() != 3 {
        return String::new();
    }
    let mut words = PAIRS.split_whitespace();
    while let (Some(three), Some(two)) = (words.next(), words.next()) {
        if three == wanted {
            return two.to_string();
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn the_table_is_well_formed_and_unique() {
        let words: Vec<&str> = PAIRS.split_whitespace().collect();
        assert_eq!(words.len() % 2, 0);
        let mut threes = BTreeSet::new();
        let mut twos = BTreeSet::new();
        for pair in words.chunks(2) {
            let (three, two) = (pair[0], pair[1]);
            assert!(
                three.len() == 3 && three.chars().all(|c| c.is_ascii_lowercase()),
                "{three}"
            );
            assert!(
                two.len() == 2 && two.chars().all(|c| c.is_ascii_uppercase()),
                "{two}"
            );
            assert!(threes.insert(three), "{three} twice");
            assert!(twos.insert(two), "{two} twice");
        }
        // ISO 3166-1 has 249 codes; Kosovo makes 250.
        assert_eq!(threes.len(), 250);
        // Listed in alpha-3 order, so a missing code is easy to spot.
        let listed: Vec<&str> = words.chunks(2).map(|p| p[0]).collect();
        let mut sorted = listed.clone();
        sorted.sort_unstable();
        assert_eq!(listed, sorted);
    }

    #[test]
    fn google_codes_become_the_ones_micaforge_stores() {
        assert_eq!(alpha2("usa"), "US");
        assert_eq!(alpha2("DEU"), "DE");
        assert_eq!(alpha2("gbr"), "GB");
        assert_eq!(alpha2("xkk"), "XK");
        assert_eq!(alpha2("zzz"), "", "Google could not tell");
        assert_eq!(alpha2(""), "");
        assert_eq!(alpha2("us"), "", "only alpha-3 is read");
    }
}
