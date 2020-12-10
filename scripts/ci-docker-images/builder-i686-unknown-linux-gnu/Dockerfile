FROM i386/debian:9.5 as builder
# This is formatted "$UID:$GID" by the docker-compose/scripts.
ARG USER

RUN apt-get update && \
    apt-get -y upgrade

RUN apt-get install -y \
        make libssl-dev cmake git \
        build-essential sudo curl
        
RUN curl -L https://cpanmin.us | perl - App::cpanminus

RUN cpanm File::Rename \
 && rename --version

RUN cd /tmp && \
  git clone https://github.com/github/cmark-gfm && \
  cd cmark-gfm && \
  git checkout 0.29.0.gfm.0 && \
  make install INSTALL_PREFIX=/usr && \
  ldconfig && \
  cd .. && \
  rm -rf cmark-gfm && \
  cmark-gfm --version

RUN sudo adduser runner
RUN sudo usermod -aG sudo runner

USER runner
RUN curl https://sh.rustup.rs -sSf | sh -s -- --no-modify-path --default-host i686-unknown-linux-gnu -y
ENV PATH=/home/runner/.cargo/bin:$PATH
RUN echo "export PATH=/home/runner/.cargo/bin:$PATH" >> ~/bashrc
ENV LIBRARY_PATH /usr/local/lib:$LIBRARY_PATH
ENV LD_LIBRARY_PATH /usr/local/lib:$LD_LIBRARY_PATH

RUN rustup update stable
RUN rustup run stable cargo install cargo-deb --target=i686-unknown-linux-gnu --version '^1.24.0'

CMD ["bash"]
