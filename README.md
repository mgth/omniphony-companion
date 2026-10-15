# Omniphony Companion

Compagnon de calibration acoustique pour [Omniphony](https://github.com/mgth/Omniphony).

**État : premier incrément hors ligne de la mission délais et niveaux.**
Le prototype traite des observations déjà estimées et propose des gains/délais.
Il ne capture pas encore de microphone, n'analyse pas de PCM et ne pilote pas
un renderer réel. La démonstration est explicitement simulée.

Le [plan de développement](PLAN.md) décrit les sept lots de la première mission
et les extensions futures : grave, caissons, réflexions et affinage global.

## Essayer

Rust 1.89 ou ultérieur. Aucun périphérique audio ni service Omniphony requis.

```sh
cargo run --locked -p companion-cli -- demo --out-dir sessions/demo
cargo run --locked -p companion-cli -- inspect --session sessions/demo/session.json
cargo run --locked -p companion-cli -- plan --session sessions/demo/session.json --out sessions/proposed.json
cargo run --locked -p companion-cli -- verify --session sessions/demo/session.json --profile sessions/proposed.json --measurements sessions/demo/verification-measurements.json --out sessions/checked.json
```

La démonstration représente trois enceintes, un caisson conservé sans correction
automatique, deux microphones et deux positions. La politique initiale aligne
uniquement les enceintes à la position de référence. Une seconde série
d'observations est générée à partir de l'état modifié du banc, puis vérifiée.
Le banc est ensuite restauré à ses réglages initiaux.

À la position de référence, le scénario passe de 2 ms à 0 ms d'écart d'arrivée
et de 5 dB à 0 dB d'écart de niveau. Ce sont des résultats déterministes du
simulateur, pas une précision mesurée en acoustique réelle.

La commande produit :

- `session.json` : installation, micros, positions et observations initiales ;
- `profile.json` : politique, état initial et valeurs absolues proposées ;
- `verification-measurements.json` : nouvelle série d'observations ;
- `verification-report.json` : écarts avant/après et statut de vérification.

Les chemins de sortie existants sont refusés. Pour rejouer la démonstration,
choisir un nouveau dossier. Les sessions locales sont ignorées par Git.

## Capacités du premier incrément

- Modèle JSON versionné avec identifiants de sorties, micros et positions.
- Validation des valeurs, références, doublons et couverture du point de référence.
- Rejet des mesures saturées, désynchronisées ou de SNR insuffisant.
- Proposition conservatrice : retarder les arrivées précoces et atténuer les
  enceintes plus fortes, en tenant compte des gains/délais déjà actifs.
- Vérification distincte sur les observations après application.
- Préparation d'application/restauration idempotente, refusant un état périmé
  ou des modifications intervenues depuis la mesure.
- Sauvegarde des documents sans écraser un fichier existant.

Les observations importées doivent déjà être comparables : même signal/niveau
d'émission, méthode de mesure du niveau, calibration micro et référence
temporelle. Les champs déclaratifs ne prouvent pas à eux seuls cette qualité.
Le futur service d'acquisition devra la mesurer et la tracer.

La première politique exige le même micro pour les observations de la position
de référence. Les autres positions et micros sont conservés pour les étapes
suivantes ; ils ne sont pas moyennés et ne participent pas au verdict actuel.

`verify` retourne 0 si les tolérances sont satisfaites, 2 si la mesure valide
échoue aux tolérances, 1 si les données ou les fichiers sont invalides.
Les tolérances par défaut (0,1 ms / 0,5 dB) sont des seuils de prototype à
valider sur matériel. Le rapport ne juge pas l'ensemble de la zone d'écoute.

## Architecture et limites

- `crates/calibration-core` : modèles, validation, propositions et vérification ;
  aucun matériel audio, réseau, accès fichier, timer ou toolkit UI.
- `crates/companion-cli` : commandes, fichiers et banc simulé.

Le cœur accepte des observations d'arrivée/niveau ; l'estimation de l'arrivée
directe à partir d'enregistrements est encore à développer. Les fonctions
d'application retournent un état cible : un futur adaptateur devra vérifier et
appliquer cet état atomiquement côté renderer. Il n'existe pas encore de
transaction réseau ni de restauration automatique après panne matérielle.

Prochaine tranche : contrat de session de mesure avec Omniphony, puis capture
micro et référence temporelle. La capture réelle, les balayages, la compensation
d'horloge, l'assistant graphique, les itérations automatiques et les corrections
acoustiques avancées restent à implémenter.

## Validation du développement

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

La CI exécute ces vérifications, la démonstration et une compilation avec Rust
1.89. Les changements passent par une branche dédiée et une PR à relire.
