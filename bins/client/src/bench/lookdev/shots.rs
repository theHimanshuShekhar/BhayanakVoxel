//! The recorded places of the shot list. Written by
//! `finder::record_the_look_dev_sites`; do not edit by hand.

use super::{CavePalette, Hue, Key, Place, Shot, Stamp};

/// The `GENERATOR_VERSION` the places were found on.
pub const RECORDED_FOR: u32 = 6;

/// The seed and radius (Voxels) of the Planet they are on, which a test holds to the client's own.
pub const RECORDED_SEED: u64 = 0xB4A7A9AC;
pub const RECORDED_RADIUS: i32 = 6400000;

pub const SHOTS: [Shot; 8] = [
    Shot {
        name: "forest floor",
        subject: "dappled forest floor at 5 cm: how small a Voxel reads, ferns and leaf litter, soft sun through a canopy",
        keys: &[
            Key {
                position: [-3763785.7589689093, 5058972.047818285, 1101514.8646341302],
                target: [-3764097.543037086, 5058726.42510782, 1101461.3839933772],
                roll: 0.0,
            },
            Key {
                position: [-3763875.9672391294, 5058893.888557877, 1101497.8466339647],
                target: [-3764187.7510040305, 5058648.266062835, 1101444.3660401169],
                roll: 0.0,
            },
        ],
        frames: 90,
        places: &[
            Place {
                stamp: Stamp::Tree,
                at: [-3763871, 5058871, 1101411],
                face: [0, 1, 0],
            },
            Place {
                stamp: Stamp::Tree,
                at: [-3763829, 5058863, 1101655],
                face: [0, 1, 0],
            },
            Place {
                stamp: Stamp::Tree,
                at: [-3763977, 5058762, 1101479],
                face: [0, 1, 0],
            },
            Place {
                stamp: Stamp::Fern,
                at: [-3763812, 5058914, 1101471],
                face: [0, 1, 0],
            },
            Place {
                stamp: Stamp::Fern,
                at: [-3763901, 5058836, 1101444],
                face: [0, 1, 0],
            },
            Place {
                stamp: Stamp::Fern,
                at: [-3763956, 5058772, 1101533],
                face: [0, 1, 0],
            },
            Place {
                stamp: Stamp::Flowers,
                at: [-3763797, 5058917, 1101523],
                face: [0, 1, 0],
            },
            Place {
                stamp: Stamp::Flowers,
                at: [-3763857, 5058873, 1101473],
                face: [0, 1, 0],
            },
            Place {
                stamp: Stamp::Moss,
                at: [-3763887, 5058838, 1101506],
                face: [0, 1, 0],
            },
        ],
        cave: None,
    },
    Shot {
        name: "canyon with haze",
        subject: "a gully with walls of slope and strata: aerial perspective down a long view, the sky seen as a strip, haze in the valley",
        keys: &[
            Key {
                position: [2176317.9694988886, 1969173.096145315, -5712161.368901898],
                target: [2176290.8589496547, 1967635.8908467982, -5712611.569783356],
                roll: 0.0,
            },
            Key {
                position: [2176265.711557725, 1968274.9378016472, -5712317.469811439],
                target: [2176238.601008521, 1966737.743102815, -5712767.667039703],
                roll: 0.1,
            },
        ],
        frames: 120,
        places: &[],
        cave: None,
    },
    Shot {
        name: "dark descent",
        subject: "daylight to dark: the auto-exposure's adaptation rate, the tone curve's toe, how much a cave shows with no light in it",
        keys: &[
            Key {
                position: [-4248808.604175764, 4789438.789660867, 75470.89775507794],
                target: [-4248432.835966013, 4789513.601468133, 75356.00020926169],
                roll: 0.0,
            },
            Key {
                position: [-4248545.566428938, 4789491.157925953, 75390.46947300657],
                target: [-4248169.798219187, 4789565.969733219, 75275.57192719032],
                roll: 0.0,
            },
            Key {
                position: [-4248019.490935287, 4789595.894456124, 75229.61290886383],
                target: [-4247643.722725537, 4789670.706263389, 75114.71536304758],
                roll: 0.0,
            },
        ],
        frames: 181,
        places: &[],
        cave: None,
    },
    Shot {
        name: "crystal cave, cyan",
        subject: "a crystal cave lit by its own emissives and nothing else: does the crystal read neon here and pastel by day, bloom, the dark between",
        keys: &[
            Key {
                position: [-4085084.7813227237, 4901549.835875221, 444140.3009468021],
                target: [-4085199.3032820486, 4901468.070692542, 443765.86914295267],
                roll: 0.0,
            },
            Key {
                position: [-4085142.042302386, 4901508.953283881, 443953.08504487737],
                target: [-4085256.5642617107, 4901427.188101202, 443578.65324102796],
                roll: 0.0,
            },
        ],
        frames: 120,
        places: &[
            Place {
                stamp: Stamp::Crystal(Hue::Cyan),
                at: [-4085142, 4901607, 444127],
                face: [0, -1, 0],
            },
            Place {
                stamp: Stamp::Crystal(Hue::Cyan),
                at: [-4085177, 4901557, 444095],
                face: [1, 0, 0],
            },
            Place {
                stamp: Stamp::Crystal(Hue::Cyan),
                at: [-4085102, 4901598, 444030],
                face: [0, -1, 0],
            },
            Place {
                stamp: Stamp::Crystal(Hue::Cyan),
                at: [-4085173, 4901488, 444021],
                face: [0, 1, 0],
            },
            Place {
                stamp: Stamp::Crystal(Hue::Cyan),
                at: [-4085150, 4901597, 443978],
                face: [0, -1, 0],
            },
            Place {
                stamp: Stamp::Crystal(Hue::Violet),
                at: [-4085067, 4901507, 444100],
                face: [0, 1, 0],
            },
            Place {
                stamp: Stamp::Crystal(Hue::Violet),
                at: [-4085102, 4901490, 444072],
                face: [0, 1, 0],
            },
            Place {
                stamp: Stamp::Crystal(Hue::Violet),
                at: [-4085075, 4901518, 444015],
                face: [-1, 0, 0],
            },
            Place {
                stamp: Stamp::Crystal(Hue::Violet),
                at: [-4085114, 4901480, 443992],
                face: [0, 1, 0],
            },
            Place {
                stamp: Stamp::Crystal(Hue::White),
                at: [-4085067, 4901586, 444063],
                face: [-1, 0, 0],
            },
            Place {
                stamp: Stamp::Crystal(Hue::White),
                at: [-4085179, 4901506, 444041],
                face: [1, 0, 0],
            },
        ],
        cave: Some(CavePalette {
            dominant: Hue::Cyan,
            accent: Hue::White,
            bed: Hue::Violet,
        }),
    },
    Shot {
        name: "crystal cave, magenta",
        subject: "a crystal cave lit by its own emissives and nothing else: does the crystal read neon here and pastel by day, bloom, the dark between",
        keys: &[
            Key {
                position: [-4147605.261727574, 4861321.6665133545, 353244.47973750316],
                target: [-4147382.871692052, 4861597.4310335405, 353058.74712995294],
                roll: 0.0,
            },
            Key {
                position: [-4147494.066709813, 4861459.548773447, 353151.61343372805],
                target: [-4147271.6766742915, 4861735.313293633, 352965.88082617783],
                roll: 0.0,
            },
        ],
        frames: 120,
        places: &[
            Place {
                stamp: Stamp::Crystal(Hue::Magenta),
                at: [-4147645, 4861396, 353240],
                face: [0, -1, 0],
            },
            Place {
                stamp: Stamp::Crystal(Hue::Magenta),
                at: [-4147617, 4861418, 353168],
                face: [1, 0, 0],
            },
            Place {
                stamp: Stamp::Crystal(Hue::Magenta),
                at: [-4147556, 4861441, 353253],
                face: [0, 0, -1],
            },
            Place {
                stamp: Stamp::Crystal(Hue::Magenta),
                at: [-4147550, 4861413, 353122],
                face: [0, 0, 1],
            },
            Place {
                stamp: Stamp::Crystal(Hue::Magenta),
                at: [-4147525, 4861517, 353188],
                face: [0, -1, 0],
            },
            Place {
                stamp: Stamp::Crystal(Hue::Amber),
                at: [-4147546, 4861328, 353216],
                face: [0, 1, 0],
            },
            Place {
                stamp: Stamp::Crystal(Hue::Amber),
                at: [-4147548, 4861345, 353185],
                face: [0, 1, 0],
            },
            Place {
                stamp: Stamp::Crystal(Hue::Amber),
                at: [-4147512, 4861375, 353222],
                face: [-1, 0, 0],
            },
            Place {
                stamp: Stamp::Crystal(Hue::Amber),
                at: [-4147489, 4861414, 353151],
                face: [0, 1, 0],
            },
            Place {
                stamp: Stamp::Crystal(Hue::Red),
                at: [-4147563, 4861379, 353282],
                face: [0, 0, -1],
            },
            Place {
                stamp: Stamp::Crystal(Hue::Red),
                at: [-4147575, 4861413, 353130],
                face: [0, 0, 1],
            },
        ],
        cave: Some(CavePalette {
            dominant: Hue::Magenta,
            accent: Hue::Red,
            bed: Hue::Amber,
        }),
    },
    Shot {
        name: "sunlit cliff",
        subject: "a rock face in the low sun: slope shading, smooth against face normals, strata colour, a long cast shadow",
        keys: &[
            Key {
                position: [-5929791.653882995, -2262693.2175210295, 943417.6203378774],
                target: [-5929967.023568953, -2262727.10647332, 942887.3989705743],
                roll: 0.0,
            },
            Key {
                position: [-5929782.438034665, -2262683.028325043, 943303.399754624],
                target: [-5929967.023568953, -2262727.10647332, 942887.3989705743],
                roll: 0.0,
            },
        ],
        frames: 90,
        places: &[
            Place {
                stamp: Stamp::Moss,
                at: [-5930093, -2262806, 942715],
                face: [-1, 0, 0],
            },
            Place {
                stamp: Stamp::Moss,
                at: [-5930111, -2262729, 942738],
                face: [-1, 0, 0],
            },
            Place {
                stamp: Stamp::Fern,
                at: [-5930155, -2262742, 942674],
                face: [-1, 0, 0],
            },
            Place {
                stamp: Stamp::Fern,
                at: [-5930148, -2262802, 942653],
                face: [-1, 0, 0],
            },
            Place {
                stamp: Stamp::Fern,
                at: [-5929803, -2262710, 943084],
                face: [-1, 0, 0],
            },
            Place {
                stamp: Stamp::Fern,
                at: [-5929813, -2262641, 943116],
                face: [-1, 0, 0],
            },
        ],
        cave: None,
    },
    Shot {
        name: "meadow",
        subject: "open grass under the hero light: flower carpets, long shadows, the sky's gradient to a peach horizon",
        keys: &[
            Key {
                position: [-3755126.741616864, 5071841.901270582, 1069523.2323716688],
                target: [-3755120.874835528, 5071999.046859054, 1068738.776608161],
                roll: 0.0,
            },
            Key {
                position: [-3755127.1931582657, 5071875.525226984, 1069366.8031666905],
                target: [-3755121.3263769313, 5072032.670763879, 1068582.3476477796],
                roll: 0.0,
            },
        ],
        frames: 90,
        places: &[
            Place {
                stamp: Stamp::Flowers,
                at: [-3755075, 5071851, 1069464],
                face: [0, 1, 0],
            },
            Place {
                stamp: Stamp::Flowers,
                at: [-3755043, 5071895, 1069371],
                face: [0, 1, 0],
            },
            Place {
                stamp: Stamp::Flowers,
                at: [-3755187, 5071814, 1069231],
                face: [0, 1, 0],
            },
            Place {
                stamp: Stamp::Moss,
                at: [-3755131, 5071806, 1069475],
                face: [0, 1, 0],
            },
            Place {
                stamp: Stamp::Moss,
                at: [-3755091, 5071844, 1069442],
                face: [0, 1, 0],
            },
            Place {
                stamp: Stamp::Moss,
                at: [-3755115, 5071837, 1069389],
                face: [0, 1, 0],
            },
            Place {
                stamp: Stamp::Moss,
                at: [-3755067, 5071882, 1069348],
                face: [0, 1, 0],
            },
            Place {
                stamp: Stamp::Tree,
                at: [-3754910, 5072073, 1068959],
                face: [0, 1, 0],
            },
        ],
        cave: None,
    },
    Shot {
        name: "pull-out to the Planet",
        subject: "the Zoom's far end: aerial perspective at every scale, the atmosphere's edge, the Planet as a disc",
        keys: &[
            Key {
                position: [-3755126.7416168656, 5071841.901270585, 1069523.2323716693],
                target: [-3749006.633795418, 5064399.812765064, 1063872.5776524642],
                roll: 0.0,
            },
            Key {
                position: [-3756053.7543827626, 5072469.814147526, 1070268.8386691443],
                target: [-3719685.9202873018, 5024173.192594589, 1056029.9775775482],
                roll: 0.0,
            },
            Key {
                position: [-3778780.7901213504, 5088186.316446373, 1088299.7399784888],
                target: [-3595841.351757494, 4842640.946278163, 1031888.2133752633],
                roll: 0.0,
            },
            Key {
                position: [-4216439.402677375, 5414042.999477251, 1417622.6984548902],
                target: [-3303553.4936183025, 4242514.960419063, 1107631.8463754512],
                roll: 0.0,
            },
            Key {
                position: [-12819441.702431828, 13319937.939955954, 6733288.378361111],
                target: [0.0, 0.0, 0.0],
                roll: 0.0,
            },
        ],
        frames: 181,
        places: &[],
        cave: None,
    },
];
