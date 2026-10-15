# Plan de développement — Omniphony Companion

## Objectif

Construire un compagnon d'Omniphony qui automatise la mesure acoustique et la
mise au point d'une installation à partir d'un ou plusieurs microphones :

**Préparer → mesurer → analyser → proposer → appliquer → remesurer → vérifier.**

Le compagnon prend en charge les sessions, les acquisitions, les analyses et
l'optimisation. Omniphony émet les signaux de mesure et applique les réglages aux
sorties physiques. Les méthodes de correction avancée resteront ouvertes : ce
plan fixe les responsabilités et les jalons, sans choisir prématurément un
algorithme pour le grave ou les réflexions.

## Mission 1 — Délais et niveaux

### Résultat attendu

L'utilisateur connecte son microphone, choisit une position de référence,
lance la mesure des enceintes et obtient des réglages de gain et de délai
vérifiés par une nouvelle acquisition. Il peut écouter avant/après, sauvegarder
explicitement le résultat ou revenir aux réglages précédents.

Les niveaux sont relatifs. Une mesure absolue de pression acoustique nécessite
une calibration de sensibilité appropriée et n'est pas une condition de cette
première mission.

Les délais sont alignés pour une position de référence. Les autres positions
servent à évaluer la robustesse et, ultérieurement, à définir un compromis de
zone. On ne promet pas des temps d'arrivée identiques partout.

Les caissons restent identifiés séparément. On conserve leurs mesures et leurs
réglages ; leur intégration acoustique avec les enceintes sera traitée dans la
mission sur le grave. Un pic de réponse de caisson ne doit pas être interprété
automatiquement comme l'arrivée directe d'une enceinte large bande.

### Lots de développement

| Lot | Travail | Critère de passage |
| --- | --- | --- |
| 1. Socle et sessions | Cœur indépendant de l'interface, modèle des installations/micros/positions/mesures/profils, sauvegarde versionnée, CLI et banc simulé. | Une session est enregistrable, rechargeable et analysable sans renderer ni matériel audio. |
| 2. Contrat avec Omniphony | Découverte des sorties physiques et de leurs limites ; état initial ; émission de signaux finis et identifiés ; confirmation des commandes ; début/fin/arrêt d'une session. | La bonne sortie émet le bon signal et le renderer arrête de lui-même une mesure abandonnée. |
| 3. Acquisition avec un micro | Entrée audio, fichier de calibration micro, réglage des niveaux, capture brute, référence temporelle et gestion des horloges indépendantes. | La répétabilité est mesurée et les acquisitions saturées, interrompues, désynchronisées ou trop bruitées sont refusées. |
| 4. Analyse délais/niveaux | Réponse impulsionnelle, estimation de l'arrivée directe et du niveau représentatif, indicateurs d'incertitude, proposition bornée de réglages. | Validation sur signaux connus et installation réelle ; les ambiguïtés d'arrivée sont détectées et présentées. |
| 5. Application et vérification | Application cohérente d'un profil, lecture de l'état réellement appliqué, nouvelle acquisition, comparaison et ajustements bornés. | Le résultat est confirmé par une nouvelle mesure ; réappliquer un profil n'empile pas les corrections. |
| 6. Assistant utilisateur | Préparation, progression, reprise ciblée, résultats avant/après, écoute comparative à niveau égal, sauvegarde et restauration. | Parcours complet sans commandes manuelles ; fermeture/minimisation de la fenêtre sans conséquence sur les délais d'arrêt. |
| 7. Plusieurs positions et micros | Déplacement guidé d'un micro, puis capture simultanée sur interface multicanale ; calibration et résultats propres à chaque micro/position. | Les mêmes données et analyses couvrent le micro déplacé et les micros fixes ; référence d'alignement explicite. |

Les lots sont des jalons fonctionnels, pas une obligation d'attendre la fin d'un
lot pour tester une tranche verticale. Le premier incrément logiciel couvre le
lot 1 et exerce une proposition/vérification sur des observations simulées.
Il ne termine ni l'acquisition réelle ni l'analyse acoustique des lots 3 et 4.

### Plusieurs micros

- Commencer par un micro, avec positions successives guidées.
- Modéliser dès le départ chaque observation par enceinte, position et micro.
- Ajouter ensuite plusieurs entrées synchronisées sur une interface commune.
- Reporter les micros USB indépendants jusqu'à validation d'une compensation
  par appareil des décalages et dérives d'horloge.
- Conserver les observations individuelles ; une moyenne de signaux bruts ne
  remplace pas une méthode explicite d'agrégation acoustique.

### Règles de mesure et d'application

- Séparer transport des commandes et temps audio : l'arrivée d'un message OSC
  ne fournit pas le temps d'émission d'un échantillon.
- Conserver une référence temporelle commune aux mesures comparées, sans
  effacer les différences de propagation à mesurer.
- Fixer et tracer les gains, traitements et routages pendant une acquisition.
  Éviter les changements automatiques de gain et les traitements du microphone.
- Identifier les sorties par des identifiants stables ; détecter tout changement
  de disposition, de fréquence d'échantillonnage ou de réglages pendant la session.
- Distinguer réglages initiaux, corrections calculées et valeurs absolues à
  appliquer, afin d'éviter les cumuls et les applications à une autre installation.
- Appliquer les réglages sans allocation ni calcul d'analyse dans le chemin
  audio ; préparer les données hors temps réel.
- Borner niveau/durée des signaux, gains et délais. Prévoir l'arrêt local côté
  renderer si le compagnon disparaît, indépendamment de l'interface graphique.
- Arrêter les itérations lorsque l'amélioration n'est plus significative par
  rapport à la répétabilité. Conserver le meilleur état vérifié.
- Distinguer essai et sauvegarde : appliquer temporairement un profil ne doit
  pas changer silencieusement la configuration persistante d'Omniphony.

## Architecture

1. **Cœur de calibration** : modèles, validation, progression d'une session,
   politiques de réglage, comparaison des résultats. Pas de dépendance UI,
   d'accès matériel, de threads ou d'adresses OSC dans ce cœur.
2. **Services d'acquisition et d'analyse** : capture, signaux, synchronisation,
   analyse des enregistrements ; tâches hors temps réel, résultats traçables.
3. **Adaptateur Omniphony** : commandes typées, état confirmé, contrôle de la
   configuration et application/restauration cohérente des profils.
4. **Interface et CLI** : pilotent les mêmes opérations. L'interface présente les
   données et les actions ; les timers et les opérations durables sont gérés
   dans les services, même lorsque la fenêtre est minimisée.

L'implémentation démarre dans un workspace Rust indépendant. Les modules
d'analyse pourront évoluer sans changer le modèle d'acquisition. Il n'est pas
nécessaire de créer immédiatement un système de plugins.

Omniphony possède déjà des gains/délais par enceinte et un test de sortie en
bruit rose. Le mode d'acquisition synchronisée nécessitera un contrat dédié : le
test actuel ne constitue pas à lui seul un protocole de calibration.

## Données à conserver

- Configuration de l'installation, rôles et identifiants des enceintes,
  routage, fréquence d'échantillonnage et réglages effectivement utilisés.
- Micros, calibration individuelle, positions et position de référence.
- Signaux émis, enregistrements bruts, référence temporelle et métadonnées
  d'acquisition, puis versions des méthodes d'analyse.
- Observations, qualité/incertitude, réglages proposés, réglages appliqués et
  résultats de vérification.

Les schémas sont versionnés. Les enregistrements et calibrations pourront être
des fichiers associés à la session, transportables sans chemins propres à une
machine. Le premier schéma conserve des observations déjà estimées ; il ne doit
jamais les présenter comme des acquisitions micro.

## Validation

Trois niveaux complémentaires :

1. Tests déterministes : décalages/gains connus, entrées invalides, mesures
   manquantes, faible qualité, changements de configuration et réapplication.
2. Banc de mesure audio : référence temporelle, dérive d'horloge, bruit,
   saturation, réflexions susceptibles de masquer l'arrivée directe.
3. Installation réelle : répétabilité, écarts avant/après, positions de contrôle,
   écoute comparative et essais d'interruption/reconnexion.

Les tolérances acoustiques seront choisies sur la base du banc et de la
répétabilité mesurée. Les seuils du simulateur ne constituent pas une promesse
de précision sur du matériel réel.

## Missions suivantes

- **Grave et caissons** : réponse dans le grave, raccords, coopération entre
  sources et régularité entre places, dans les limites de chaque enceinte.
- **Réflexions et comportement temporel** : identifier les phénomènes
  reproductibles et corrigibles, puis évaluer les corrections sur la zone
  d'écoute et à l'écoute.
- **Affinage global** : courbes cibles, préférences, compromis de zone, profils
  et stratégies d'optimisation selon les résultats expérimentaux.

Ces missions réutilisent les acquisitions et la boucle de vérification ; leurs
algorithmes et choix IIR/FIR/MIMO restent à décider séparément.

## Livraison et suivi

- Une branche `feat/*` ou `fix/*` par sujet et une PR avec CI verte et revue.
- Aucun développement de fonctionnalité directement sur `main`.
- Les éventuels changements d'Omniphony auront leur propre workflow et PR ;
  le worktree d'intégration ne servira pas à leur implémentation.
- Le présent plan est conservé dans le dépôt du compagnon. `README.md` décrit
  précisément les fonctions déjà exécutables et les limites du prototype.

## Références de départ

- [Cecchi, Carini, Spors — Room Response Equalization, revue (2018)](https://arts.units.it/handle/11368/2933738)
- [Farina — mesure par balayage sinusoïdal (2000)](https://aes.org/publications/elibrary/elibrary-page/?id=10211)
- [Kirkeby et Nelson — inversion régularisée (1999)](https://aes.org/publications/elibrary-page/?id=12098)
- [Welti et Devantier — optimisation multicaisson (2006)](https://aes.org/publications/elibrary/elibrary-page/?id=13680)
- [REW — mesures et références temporelles](https://www.roomeqwizard.com/help/help_en-GB/html/makingmeasurements.html)
- [DRC-FIR — documentation des méthodes](https://drc-fir.sourceforge.net/doc/drc.html)
- [CamillaDSP — moteur et documentation](https://github.com/HEnquist/camilladsp)
